//! A live agent session rendered inside the TUI.
//!
//! `ssh` runs under a pty we own, its output is fed to a host-side terminal
//! emulator, and the emulated screen is drawn into the right-hand pane. Keys
//! typed while the pane has focus are encoded and written back to the pty, so
//! the agent's own TUI behaves as if it had the terminal — which, as far as it
//! can tell, it does.
//!
//! Why a pty at all: a coding agent draws a full-screen interface and asks the
//! terminal for its size. Piping stdout would give it neither, and it would
//! degrade to line mode or refuse to start.
//!
//! Detaching drops the session but does not stop the work — the agent keeps
//! running on the VM, which is the whole point of a durable box. Closing the
//! session on purpose is what sleeps the agent, and that is the caller's call,
//! not this module's.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

use crate::commands::ssh::native;

/// A durable session name for a new session.
///
/// Ours to choose: the relay creates the session when the name is unknown, and
/// having chosen it we can list, reattach to, and recognise the session later.
/// The suffix keeps a second `claude` on the same agent distinct from the first.
pub fn durable_name(harness: &str) -> String {
    use rand::Rng;
    let suffix: String = (0..6)
        .map(|_| {
            const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
            ALPHABET[rand::thread_rng().gen_range(0..ALPHABET.len())] as char
        })
        .collect();
    format!("{harness}-{suffix}")
}

/// The reply to a device-status-report cursor-position query (`ESC[6n`),
/// found anywhere in a chunk of remote output — `Some` iff the query is
/// there.
///
/// The query is how a program without a trustworthy `ioctl` answer (this
/// pane's remote side is a real pty, but a program can still choose to probe
/// rather than assume) works out where the cursor already is; some terminal
/// setup code — `railway-agent-tui`'s among them — sends it and blocks on a
/// reply before drawing anything. The query lives entirely inside the byte
/// stream this emulator parses: nothing forwards it to the real terminal this
/// pane itself is drawn in, so unless the emulator answers on the query's
/// behalf, the remote program hangs until it gives up. `ESC[row;colR`,
/// 1-indexed, is what a real terminal would have sent back — read off the
/// emulator's own idea of the cursor position after this chunk lands, so it
/// reflects everything the chunk itself just drew.
fn dsr_reply(chunk: &[u8], screen: &vt100::Screen) -> Option<Vec<u8>> {
    const QUERY: &[u8] = b"\x1b[6n";
    chunk.windows(QUERY.len()).any(|w| w == QUERY).then(|| {
        let (row, col) = screen.cursor_position();
        format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
    })
}

/// Track and answer the kitty keyboard protocol inside the pane's stream.
///
/// A harness that wants unambiguous keys (shift+enter as a newline, most
/// visibly) queries with `CSI ? u`, and only enables the protocol when a
/// reply comes back — which, inside this emulator, nothing sent until now,
/// so every harness fell back to legacy keys where shift+enter and enter are
/// the same byte. Answering the query (with the current flags) and watching
/// for the push (`CSI > flags u`) / pop (`CSI < u`) that follow lets
/// [`Session::send_key`] know when the modified-Enter CSI-u encodings will
/// be understood on the far side.
///
/// Scanning is chunk-wise, like [`dsr_reply`]: a sequence split across two
/// reads is missed, which costs one retry of a query, not correctness.
fn kitty_scan(chunk: &[u8], kitty: &AtomicBool) -> Option<Vec<u8>> {
    let mut reply = None;
    let mut i = 0;
    while let Some(at) = chunk[i..].windows(2).position(|w| w == b"\x1b[") {
        let seq = &chunk[i + at + 2..];
        let Some(end) = seq.iter().position(|b| *b == b'u') else {
            break;
        };
        match seq.first() {
            // Query: answer with the flags in effect, like a real terminal.
            Some(b'?') if seq[1..end].iter().all(u8::is_ascii_digit) => {
                let flags = u8::from(kitty.load(Ordering::Relaxed));
                reply = Some(format!("\x1b[?{flags}u").into_bytes());
            }
            // Push: the protocol is on iff any flag bit is set.
            Some(b'>') if seq[1..end].iter().all(u8::is_ascii_digit) => {
                let flags: u32 = std::str::from_utf8(&seq[1..end])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                kitty.store(flags != 0, Ordering::Relaxed);
            }
            // Pop: back to legacy keys. One level of depth is all the
            // harnesses use; a counter would be pretending to more fidelity
            // than chunk-wise scanning has anyway.
            Some(b'<') if seq[1..end].iter().all(u8::is_ascii_digit) => {
                kitty.store(false, Ordering::Relaxed);
            }
            _ => {}
        }
        // Step past the introducer only: the found `u` may belong to plain
        // text far ahead, and skipping there would jump over real sequences.
        i += at + 2;
    }
    reply
}

/// A running `ssh` under a pty, plus the emulator that makes sense of it.
pub struct Session {
    pub agent_id: String,
    pub agent_name: String,
    /// The harness this session was started with, when we started it.
    #[allow(dead_code)]
    pub harness: String,
    /// The durable session this pane is attached to.
    pub durable_name: String,
    /// How this pane connected, kept so the same session can be reopened
    /// full-screen without rebuilding the relay plumbing.
    pub ssh_target: String,
    pub identity: Option<std::path::PathBuf>,
    pub relay_opts: Vec<String>,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Shared with the reader thread, which also writes to it — a synthetic
    /// cursor-position reply (see [`dsr_reply`]) has to go back over the same
    /// pty the keyboard does, and `take_writer` can only be called once.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// Set by the reader thread when ssh's output ends — the session is over
    /// even though the child may take another moment to reap.
    ended: Arc<AtomicBool>,
    /// This pane attached to a session that already existed, rather than
    /// starting one. Only an attach can go silent (see [`Self::stalled`]).
    reattach: bool,
    /// When the pane connected, for the stall clock.
    spawned_at: std::time::Instant,
    /// Set by the reader thread on the first byte. An attach that never sets
    /// this is talking to a session whose process is gone.
    got_output: Arc<AtomicBool>,
    /// The remote program pushed the kitty keyboard protocol (see
    /// [`kitty_scan`]), so modified Enter goes out CSI-u encoded.
    kitty_keys: Arc<AtomicBool>,
    /// Last size pushed to the pty, so a redraw at the same size is free.
    size: (u16, u16),
    /// Rows scrolled back from the live view. Typing snaps back to 0 — nobody
    /// wants to type into history.
    scroll: usize,
}

impl Session {
    /// Write straight to the pty — keystrokes, pointer reports, and the
    /// reader thread's own DSR replies all go through this one shared writer.
    fn write_raw(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    /// Spawn `ssh` under a pty and start reading it.
    ///
    /// `notify` fires whenever new output has been folded into the emulator, so
    /// the event loop can redraw without polling.
    // Every one of these is a distinct fact about the session — collapsing
    // them into a struct would only move the same list one line up.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        agent_id: String,
        agent_name: String,
        harness: String,
        ssh_target: &str,
        identity: Option<&std::path::Path>,
        relay_opts: &[String],
        remote_cmd: &str,
        // True when `durable_session` names a session that already exists.
        reattach: bool,
        // The durable session to run in: an existing name reattaches, a new one
        // is created by the relay.
        durable_session: &str,
        rows: u16,
        cols: u16,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let pty = NativePtySystem::default()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("Failed to allocate a pty for the agent session")?;

        let mut cmd = CommandBuilder::new("ssh");
        // `-tt` forces a remote pty even though our own stdin is not a
        // terminal from ssh's point of view; without it the agent gets a pipe
        // and refuses to draw.
        cmd.arg("-tt");
        // The relay may listen off 22, and the target is a *username* on the
        // relay host rather than a hostname — both come from the same helpers
        // the rest of the CLI's ssh paths use.
        for arg in native::relay_port_args() {
            cmd.arg(arg);
        }
        for opt in relay_opts {
            cmd.arg(opt);
        }
        if let Some(identity) = identity {
            cmd.arg("-i");
            cmd.arg(identity);
        }
        // Resuming is a relay concern: it intercepts these env keys and hands
        // back the existing session's screen instead of starting anything, so
        // the command is deliberately omitted.
        cmd.arg("-o");
        cmd.arg(format!(
            "SetEnv RAILWAY_DURABLE_SESSION_NAME={durable_session}"
        ));
        cmd.arg(native::relay_destination(ssh_target));
        // Reattaching must not re-run the command — the relay hands back the
        // existing screen, and a command here would start a second one inside
        // it. A fresh session gets the harness; a resumed one gets nothing.
        if !reattach {
            cmd.arg(remote_cmd);
        }
        // The emulator understands xterm sequences and the relay does not
        // forward COLORTERM, so both are stated here rather than inherited.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pty
            .slave
            .spawn_command(cmd)
            .context("Failed to start ssh for the agent session")?;
        // The slave handle must go before the reader starts, or the pty never
        // reports EOF when ssh exits and the reader thread parks forever.
        drop(pty.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 4000)));
        let ended = Arc::new(AtomicBool::new(false));
        let got_output = Arc::new(AtomicBool::new(false));
        let kitty_keys = Arc::new(AtomicBool::new(false));
        let mut reader = pty
            .master
            .try_clone_reader()
            .context("Failed to read the agent session")?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(
            pty.master
                .take_writer()
                .context("Failed to write to the agent session")?,
        ));

        {
            let parser = parser.clone();
            let ended = ended.clone();
            let writer = writer.clone();
            let got_output = got_output.clone();
            let kitty_keys = kitty_keys.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            got_output.store(true, Ordering::Relaxed);
                            let mut replies =
                                kitty_scan(&buf[..n], &kitty_keys).unwrap_or_default();
                            if let Some(dsr) = parser.lock().ok().and_then(|mut parser| {
                                parser.process(&buf[..n]);
                                dsr_reply(&buf[..n], parser.screen())
                            }) {
                                replies.extend_from_slice(&dsr);
                            }
                            if !replies.is_empty() {
                                if let Ok(mut writer) = writer.lock() {
                                    let _ = writer.write_all(&replies);
                                    let _ = writer.flush();
                                }
                            }
                            notify();
                        }
                    }
                }
                ended.store(true, Ordering::Relaxed);
                notify();
            });
        }

        Ok(Self {
            agent_id,
            agent_name,
            harness,
            durable_name: durable_session.to_string(),
            ssh_target: ssh_target.to_string(),
            identity: identity.map(|p| p.to_path_buf()),
            relay_opts: relay_opts.to_vec(),
            parser,
            writer,
            child,
            master: pty.master,
            ended,
            reattach,
            spawned_at: std::time::Instant::now(),
            got_output,
            kitty_keys,
            size: (rows, cols),
            scroll: 0,
        })
    }

    /// How long an attach may stay silent before the pane says so.
    pub const STALL_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

    /// An attach that has produced nothing, for long enough to say so.
    ///
    /// Only reattaches count: a fresh launch always prints (provisioning, the
    /// harness banner), so silence there is just a slow start. An attach is
    /// silent exactly when the durable session's process is gone — the relay
    /// resolves the name, streams nothing, and never will. The platform can
    /// keep reporting such a session as running after its agent slept, so
    /// this is the pane's own way of noticing.
    pub fn stalled(&self) -> bool {
        self.reattach
            && !self.got_output.load(Ordering::Relaxed)
            && !self.ended()
            && self.spawned_at.elapsed() >= Self::STALL_AFTER
    }

    /// Time until [`Self::stalled`] would first flip, so the event loop can
    /// schedule one redraw for it. `None` when it can't stall or already has.
    pub fn stall_remaining(&self) -> Option<std::time::Duration> {
        if !self.reattach || self.got_output.load(Ordering::Relaxed) || self.ended() {
            return None;
        }
        Self::STALL_AFTER.checked_sub(self.spawned_at.elapsed())
    }

    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }

    /// Resize both the emulator and the pty. Doing only one leaves the agent
    /// drawing to a screen of a different shape than the one being rendered.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
            // Resizing can reflow rows between the screen and history; read
            // the offset back so the held position stays whatever the
            // emulator says the view now is.
            self.scroll = parser.screen().scrollback();
        }
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// The last thing this session printed, as one line.
    ///
    /// Read from the bottom of the emulated screen upwards, skipping blanks and
    /// the agent's own prompt furniture — what a status card wants is the last
    /// thing that was *said*, not the empty input line under it.
    pub fn last_line(&self) -> Option<String> {
        self.with_screen(|screen| {
            let (rows, _) = screen.size();
            (0..rows).rev().find_map(|row| {
                let text: String = screen.contents_between(row, 0, row, u16::MAX);
                let trimmed = text.trim();
                let bare_prompt = trimmed
                    .trim_start_matches(['>', '$', '#', '·', '❯', '▌', '│', '╰', '─'])
                    .trim()
                    .is_empty();
                (!bare_prompt).then(|| trimmed.to_string())
            })
        })
        .flatten()
    }

    /// The URL under a cell of the emulated screen, if there is one.
    ///
    /// The pane captures the mouse, so the terminal's own link handling never
    /// sees the click — this is what puts it back.
    ///
    /// Reassembles the *logical* line first. An OAuth or device-code URL is
    /// routinely longer than the pane is wide, so the interesting case is
    /// always a link split across two or three rows; matching within one row
    /// finds only the fragment up to the wrap, which is not a URL anybody can
    /// open. Text only: vt100 0.15 does not surface OSC 8 hyperlinks, so a link
    /// whose visible text is not the URL cannot be found this way.
    pub fn url_at(&self, row: u16, col: u16) -> Option<String> {
        self.with_screen(|screen| {
            let (rows, cols) = screen.size();
            if row >= rows || col >= cols {
                return None;
            }
            // The run of rows the emulator says are one wrapped line.
            let mut start = row;
            while start > 0 && screen.row_wrapped(start - 1) {
                start -= 1;
            }
            let mut end = row;
            while end + 1 < rows && screen.row_wrapped(end) {
                end += 1;
            }

            // Built cell by cell rather than with `contents_between`, so the
            // click's index into the joined text is exact — a blank cell has to
            // occupy a column, or every position after it is off by one.
            let mut text = String::new();
            let mut index = None;
            for r in start..=end {
                for c in 0..cols {
                    if r == row && c == col {
                        index = Some(text.chars().count());
                    }
                    match screen.cell(r, c).map(|cell| cell.contents()) {
                        Some(s) if !s.is_empty() => text.push_str(s),
                        // Empty, or a wide character's second cell: still a
                        // column.
                        _ => text.push(' '),
                    }
                }
            }
            url_in(&text, index?)
        })?
    }

    /// Read the emulated screen. Held briefly — the reader thread wants the
    /// same lock.
    pub fn with_screen<T>(&self, f: impl FnOnce(&vt100::Screen) -> T) -> Option<T> {
        self.parser.lock().ok().map(|parser| f(parser.screen()))
    }

    /// Is the application in here handling the mouse itself?
    ///
    /// A coding agent with clickable output — "click here to copy", a menu you
    /// can point at — turns mouse reporting on and expects the events. Until
    /// they are forwarded, that output is inert.
    pub fn wants_mouse(&self) -> bool {
        self.with_screen(|screen| screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None)
            .unwrap_or(false)
    }

    /// Hand a pointer event to the application, if it asked for that kind.
    ///
    /// Returns whether it was sent, so the caller knows whether the click is
    /// still theirs to act on. Motion goes only to applications that asked for
    /// button tracking; the press-only modes would be confused by it.
    pub fn pointer(&mut self, kind: Pointer, at: (u16, u16)) -> bool {
        use vt100::MouseProtocolMode as Mode;
        let Some((mode, encoding)) = self.with_screen(|screen| {
            (
                screen.mouse_protocol_mode(),
                screen.mouse_protocol_encoding(),
            )
        }) else {
            return false;
        };
        let wanted = match (mode, kind) {
            (Mode::None, _) => false,
            // X10 mode hears about presses and nothing else.
            (Mode::Press, Pointer::Press) => true,
            (Mode::Press, _) => false,
            (Mode::PressRelease, Pointer::Drag) => false,
            (_, _) => true,
        };
        if !wanted {
            return false;
        }
        // Not through `send`: this is not typing, and it must not cancel a
        // scrollback the way a keystroke does.
        self.write_raw(&pointer_report(kind, at, encoding));
        true
    }

    /// Scroll back through the emulator's history.
    ///
    /// The only ceiling is the history that actually exists: the emulator
    /// clamps the offset to it, so ask for the position and read back where
    /// it settled. (vt100 0.15 could not compose a view more than one screen
    /// deep — a clamp used to sit here working around that.)
    pub fn scroll_by(&mut self, delta: isize) {
        let Ok(mut parser) = self.parser.lock() else {
            return;
        };
        let wanted = (self.scroll as isize).saturating_add(delta).max(0) as usize;
        parser.screen_mut().set_scrollback(wanted);
        self.scroll = parser.screen().scrollback();
    }

    pub fn scrolled_back(&self) -> bool {
        self.scroll > 0
    }

    /// Scroll the pane, whichever way this session can be scrolled.
    ///
    /// Three cases, because "scroll" means something different depending on
    /// what is running:
    ///
    /// - the application asked for mouse reporting: send it a real wheel
    ///   event, so *its* viewport scrolls. This is the case for a coding agent,
    ///   and the reason arrow keys are wrong — a harness reads those as
    ///   history, so the wheel walked through old prompts instead of scrolling;
    /// - the alternate screen with no mouse reporting: nothing sensible to do.
    ///   Nothing scrolls off it, so there is no history here or there;
    /// - anything else (a plain shell): the emulator's own scrollback.
    ///
    /// `at` is the cell the pointer is over, one-based within the pane, which
    /// is what the wheel report carries.
    pub fn scroll(&mut self, up: bool, lines: usize, at: (u16, u16)) {
        let Some((mode, encoding, alternate)) = self.with_screen(|screen| {
            (
                screen.mouse_protocol_mode(),
                screen.mouse_protocol_encoding(),
                screen.alternate_screen(),
            )
        }) else {
            return;
        };

        if mode != vt100::MouseProtocolMode::None {
            let mut out = Vec::new();
            for _ in 0..lines {
                out.extend_from_slice(&wheel_report(up, at, encoding));
            }
            // Not through `send`: this is not typing, and it must not snap the
            // view back to live.
            self.write_raw(&out);
            return;
        }
        if alternate {
            return;
        }
        self.scroll_by(if up {
            lines as isize
        } else {
            -(lines as isize)
        });
    }

    /// Return to the live view.
    fn scroll_to_live(&mut self) {
        if self.scroll == 0 {
            return;
        }
        self.scroll = 0;
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_scrollback(0);
        }
    }

    /// Is there anything scrolling can do here?
    pub fn scrollable(&self) -> bool {
        self.with_screen(|screen| {
            screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None
                || !screen.alternate_screen()
        })
        .unwrap_or(false)
    }

    pub fn send(&mut self, bytes: &[u8]) {
        // Typing is a statement of intent to be at the bottom.
        self.scroll_to_live();
        self.write_raw(bytes);
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        if let Some(bytes) = encode_key_for(key, self.kitty_keys.load(Ordering::Relaxed)) {
            self.send(&bytes);
        }
    }

    /// Stop the local half. The agent and whatever it is running stay up on the
    /// VM — killing ssh detaches, it does not tidy up.
    pub fn detach(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The URL covering `col` in `line`, if the word there is one.
///
/// Whitespace-delimited, because that is how a URL sits in terminal output, and
/// then trimmed of the punctuation that tends to follow one in prose. A URL
/// wrapped across two rows is found only up to the break — the emulator has no
/// record that the two halves were ever one line.
fn url_in(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() || chars[col].is_whitespace() {
        return None;
    }
    let start = chars[..col]
        .iter()
        .rposition(|c| c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = chars[col..]
        .iter()
        .position(|c| c.is_whitespace())
        .map(|i| col + i)
        .unwrap_or(chars.len());

    let word: String = chars[start..end].iter().collect();
    // Punctuation around a link belongs to the sentence, not the link — a URL
    // in prose is as often `(https://…)` or `<https://…>` as it is bare.
    let word = word.trim_start_matches(['(', '[', '{', '<', '\'', '"']);
    let mut url = word.trim_end_matches(['.', ',', ';', ':', '!', '?', '>', '\'', '"']);
    while url.ends_with(')') && url.matches('(').count() < url.matches(')').count() {
        url = &url[..url.len() - 1];
    }
    while url.ends_with(']') && url.matches('[').count() < url.matches(']').count() {
        url = &url[..url.len() - 1];
    }

    let known = url.starts_with("http://") || url.starts_with("https://");
    // Something has to follow the scheme, or "https://" on its own is a link.
    (known && url.len() > "https://".len()).then(|| url.to_string())
}

#[cfg(test)]
impl Session {
    /// A session backed by a local `cat` instead of ssh, so the state machine
    /// around sessions can be tested without a relay or a network.
    pub fn for_test(agent_id: &str, agent_name: &str) -> Result<Self> {
        let pty = NativePtySystem::default().openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let child = pty.slave.spawn_command(CommandBuilder::new("cat"))?;
        drop(pty.slave);
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 4000)));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pty.master.take_writer()?));

        // The same reader the real session runs. Without it the emulator never
        // sees a byte, and a test against this fixture would be testing
        // nothing at all.
        let mut reader = pty.master.try_clone_reader()?;
        {
            let parser = parser.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut parser) = parser.lock() {
                        parser.process(&buf[..n]);
                    }
                }
            });
        }
        Ok(Self {
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            harness: "claude".to_string(),
            durable_name: "test".to_string(),
            ssh_target: "agent:test:test".to_string(),
            identity: None,
            relay_opts: Vec::new(),
            parser,
            writer,
            child,
            master: pty.master,
            ended: Arc::new(AtomicBool::new(false)),
            reattach: false,
            spawned_at: std::time::Instant::now(),
            got_output: Arc::new(AtomicBool::new(true)),
            kitty_keys: Arc::new(AtomicBool::new(false)),
            size: (24, 80),
            scroll: 0,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.detach();
    }
}

/// A wheel event as the terminal would report it.
///
/// Buttons 64 and 65 are wheel up and down. SGR is unambiguous and what modern
/// applications ask for; the default encoding offsets everything by 32 and
/// cannot express a coordinate past 223, which is why it is the fallback rather
/// than the choice.
/// A pointer event to hand to the application in the session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pointer {
    Press,
    /// Moved with the button held.
    Drag,
    Release,
}

/// The SGR/legacy button code for a left-button event.
fn pointer_button(kind: Pointer) -> u16 {
    match kind {
        // The legacy encoding has no separate release code, so a release is
        // button 3 there and button 0 with a final `m` under SGR.
        Pointer::Press | Pointer::Release => 0,
        Pointer::Drag => 32,
    }
}

fn pointer_report(
    kind: Pointer,
    at: (u16, u16),
    encoding: vt100::MouseProtocolEncoding,
) -> Vec<u8> {
    let button = pointer_button(kind);
    let (col, row) = (at.0.max(1), at.1.max(1));
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let final_byte = if kind == Pointer::Release { 'm' } else { 'M' };
            format!("\x1b[<{button};{col};{row}{final_byte}").into_bytes()
        }
        _ => {
            let clamp = |v: u16| (v.min(223) + 32) as u8;
            let button = if kind == Pointer::Release { 3 } else { button };
            vec![
                0x1b,
                b'[',
                b'M',
                (button + 32) as u8,
                clamp(col),
                clamp(row),
            ]
        }
    }
}

fn wheel_report(up: bool, at: (u16, u16), encoding: vt100::MouseProtocolEncoding) -> Vec<u8> {
    let button: u16 = if up { 64 } else { 65 };
    let (col, row) = (at.0.max(1), at.1.max(1));
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => format!("\x1b[<{button};{col};{row}M").into_bytes(),
        _ => {
            let clamp = |v: u16| (v.min(223) + 32) as u8;
            vec![
                0x1b,
                b'[',
                b'M',
                (button + 32) as u8,
                clamp(col),
                clamp(row),
            ]
        }
    }
}

/// [`encode_key`], plus the encodings that only exist once the remote side
/// has pushed the kitty keyboard protocol (see [`kitty_scan`]).
///
/// Modified Enter is the whole reason this split exists: legacy terminals
/// send `\r` for shift+enter, ctrl+enter and plain enter alike, which is why
/// shift+enter never made a newline in a harness. The CSI-u form says which
/// one it was — but only to a program expecting it, so it is sent only after
/// the push. Anything else would read the escape sequence as typed text.
///
/// Separate from `Session` so the choice is testable without a pty: Windows'
/// ConPTY interprets escape sequences instead of forwarding them, so a
/// round-trip test can only run on unix.
fn encode_key_for(key: KeyEvent, kitty: bool) -> Option<Vec<u8>> {
    if kitty
        && key.code == KeyCode::Enter
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL)
    {
        // The kitty modifier field is a 1-based bitfield: shift 1, alt 2,
        // ctrl 4. 13 is Enter's codepoint.
        let m = 1
            + u8::from(key.modifiers.contains(KeyModifiers::SHIFT))
            + 2 * u8::from(key.modifiers.contains(KeyModifiers::ALT))
            + 4 * u8::from(key.modifiers.contains(KeyModifiers::CONTROL));
        return Some(format!("\x1b[13;{m}u").into_bytes());
    }
    encode_key(key)
}

/// Encode a key event as the bytes a terminal would send.
///
/// Enough of xterm's vocabulary for a coding agent: text, the control chords
/// they bind, arrows and navigation in their normal (non-application) forms,
/// and function keys. `None` means "nothing a terminal would have sent", which
/// is the right answer for a bare modifier press.
pub fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    let mut out: Vec<u8> = match key.code {
        KeyCode::Char(c) if ctrl => {
            // Ctrl maps the letter block to 0x01..0x1a, plus the handful of
            // punctuation chords terminals define.
            let byte = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => (c as u8) - b'a' + 1,
                '@' | ' ' => 0,
                '[' => 27,
                '\\' => 28,
                ']' => 29,
                '^' => 30,
                '_' | '?' => 31,
                _ => return None,
            };
            vec![byte]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::F(n @ 1..=4) => vec![0x1b, b'O', b'P' + (n - 1)],
        KeyCode::F(n @ 5..=12) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            format!("\x1b[{code}~").into_bytes()
        }
        _ => return None,
    };

    // Alt is a leading ESC, the convention every terminal emulator sends and
    // every readline-alike expects.
    if alt {
        out.insert(0, 0x1b);
    }
    // Shift is already carried by the character itself; it only matters for the
    // keys that have no character, and of those only Tab has a distinct code.
    let _ = shift;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The kitty keyboard protocol dance, as a harness does it: query, get
    /// an answer, push, and only then is modified Enter CSI-u encoded.
    #[test]
    fn kitty_query_push_and_pop_are_tracked() {
        let kitty = AtomicBool::new(false);

        // The query gets the current flags back — none yet.
        let reply = kitty_scan(b"setup\x1b[?u more", &kitty);
        assert_eq!(reply.as_deref(), Some(b"\x1b[?0u".as_slice()));
        assert!(!kitty.load(Ordering::Relaxed));

        // Push turns it on; the next query reports it.
        assert_eq!(kitty_scan(b"\x1b[>1u", &kitty), None);
        assert!(kitty.load(Ordering::Relaxed));
        let reply = kitty_scan(b"\x1b[?u", &kitty);
        assert_eq!(reply.as_deref(), Some(b"\x1b[?1u".as_slice()));

        // A push of zero flags is legacy keys by another name.
        kitty_scan(b"\x1b[>0u", &kitty);
        assert!(!kitty.load(Ordering::Relaxed));

        // Pop turns it off.
        kitty_scan(b"\x1b[>1u", &kitty);
        kitty_scan(b"\x1b[<1u", &kitty);
        assert!(!kitty.load(Ordering::Relaxed));

        // Ordinary output — including a stray `u` — changes nothing.
        assert_eq!(kitty_scan(b"\x1b[38;5;2mgreen up\x1b[0m", &kitty), None);
        assert!(!kitty.load(Ordering::Relaxed));
    }

    /// Shift+enter reaches the harness as a newline only via the kitty
    /// encoding — legacy `\r` for every modified Enter is exactly the
    /// ambiguity being fixed.
    #[test]
    fn modified_enter_is_csi_u_encoded_once_kitty_is_active() {
        let enter = |m| KeyEvent::new(KeyCode::Enter, m);
        let bytes = |key, kitty| encode_key_for(key, kitty).unwrap();

        // Each modifier its own bit, and combinations sum.
        assert_eq!(bytes(enter(KeyModifiers::SHIFT), true), b"\x1b[13;2u");
        assert_eq!(bytes(enter(KeyModifiers::ALT), true), b"\x1b[13;3u");
        assert_eq!(bytes(enter(KeyModifiers::CONTROL), true), b"\x1b[13;5u");
        assert_eq!(
            bytes(enter(KeyModifiers::SHIFT | KeyModifiers::CONTROL), true),
            b"\x1b[13;6u"
        );

        // Unmodified Enter is `\r` either way: it is not ambiguous, and a
        // harness reading CSI-u for it would never see a plain submit.
        assert_eq!(bytes(enter(KeyModifiers::NONE), true), b"\r");
        assert_eq!(bytes(enter(KeyModifiers::NONE), false), b"\r");

        // No push, no CSI-u: to a legacy program the escape sequence is
        // typed text, which is worse than the ambiguity it replaces.
        assert_eq!(bytes(enter(KeyModifiers::SHIFT), false), b"\r");

        // Everything else routes through the legacy encoder untouched.
        assert_eq!(bytes(key(KeyCode::Char('a')), true), b"a");
        assert_eq!(bytes(key(KeyCode::Tab), true), b"\t");
    }

    /// Unix only: this needs an escape sequence to survive the trip through
    /// the pty, and Windows' ConPTY interprets those for itself instead of
    /// passing them along, so the emulator never sees what was sent. Plain
    /// text round-trips fine, which is why the rest of these run everywhere.
    #[cfg(unix)]
    #[test]
    fn the_kitty_encoding_goes_out_on_the_wire() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.kitty_keys.store(true, Ordering::Relaxed);
        session.send_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        for _ in 0..40 {
            if session
                .with_screen(|s| s.contents().contains("[13;2u"))
                .unwrap_or(false)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // `cat` echoes what it was sent, so the emulator shows the sequence
        // (ESC swallowed) — proof the CSI-u bytes went out, not `\r`.
        assert!(
            session
                .with_screen(|s| s.contents().contains("[13;2u"))
                .unwrap_or(false),
            "expected the kitty encoding on the wire"
        );
    }

    #[test]
    fn text_and_enter_encode_as_themselves() {
        assert_eq!(encode_key(key(KeyCode::Char('a'))).unwrap(), b"a");
        assert_eq!(encode_key(key(KeyCode::Char('~'))).unwrap(), "~".as_bytes());
        // Carriage return, not newline: that is what a terminal sends, and a
        // readline prompt ignores \n.
        assert_eq!(encode_key(key(KeyCode::Enter)).unwrap(), b"\r");
        assert_eq!(encode_key(key(KeyCode::Backspace)).unwrap(), &[0x7f]);
    }

    /// Ctrl-C has to reach the agent as an interrupt; anything else and there
    /// is no way to stop a runaway task inside the pane.
    #[test]
    fn control_chords_encode_to_control_bytes() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(encode_key(ctrl('c')).unwrap(), &[0x03]);
        assert_eq!(encode_key(ctrl('d')).unwrap(), &[0x04]);
        assert_eq!(encode_key(ctrl('a')).unwrap(), &[0x01]);
        assert_eq!(encode_key(ctrl('z')).unwrap(), &[0x1a]);
        // Uppercase is the same chord.
        assert_eq!(encode_key(ctrl('C')).unwrap(), &[0x03]);
    }

    #[test]
    fn arrows_and_function_keys_use_xterm_sequences() {
        assert_eq!(encode_key(key(KeyCode::Up)).unwrap(), b"\x1b[A");
        assert_eq!(encode_key(key(KeyCode::Left)).unwrap(), b"\x1b[D");
        assert_eq!(encode_key(key(KeyCode::PageUp)).unwrap(), b"\x1b[5~");
        assert_eq!(encode_key(key(KeyCode::F(1))).unwrap(), b"\x1bOP");
        assert_eq!(encode_key(key(KeyCode::F(5))).unwrap(), b"\x1b[15~");
        assert_eq!(encode_key(key(KeyCode::BackTab)).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn alt_prefixes_an_escape() {
        let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(encode_key(alt_b).unwrap(), b"\x1bb");
    }

    #[test]
    fn keys_a_terminal_would_not_send_produce_nothing() {
        assert!(encode_key(key(KeyCode::Null)).is_none());
        assert!(encode_key(KeyEvent::new(KeyCode::CapsLock, KeyModifiers::NONE)).is_none());
    }

    #[test]
    fn a_url_is_found_under_any_of_its_characters() {
        let line = "  see https://railway.com/project/abc for the deploy";
        let url = "https://railway.com/project/abc";
        let first = line.find(url).unwrap();
        for col in first..first + url.len() {
            assert_eq!(url_in(line, col).as_deref(), Some(url), "at {col}");
        }
        // And nowhere else on the line.
        assert_eq!(url_in(line, 0), None);
        assert_eq!(url_in(line, 2), None, "\"see\" is not a link");
        assert_eq!(url_in(line, line.len() - 2), None);
    }

    /// Punctuation after a link belongs to the sentence.
    #[test]
    fn trailing_punctuation_is_not_part_of_the_link() {
        for (line, want) in [
            ("open https://railway.com.", "https://railway.com"),
            ("open https://railway.com,", "https://railway.com"),
            ("(see https://railway.com)", "https://railway.com"),
            ("[https://railway.com]", "https://railway.com"),
        ] {
            let col = line.find("https").unwrap() + 3;
            assert_eq!(url_in(line, col).as_deref(), Some(want), "{line}");
        }

        // A bracket the URL itself needs survives, because it is balanced.
        let line = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        assert_eq!(url_in(line, 10).as_deref(), Some(line));
    }

    /// Only real links, and only complete ones.
    #[test]
    fn non_links_are_left_alone() {
        assert_eq!(url_in("just some words", 5), None);
        assert_eq!(url_in("ftp://files.example.com", 4), None, "not a web link");
        assert_eq!(url_in("https://", 2), None, "a scheme is not a link");
        assert_eq!(url_in("railway.com", 3), None, "no scheme, no click");
        assert_eq!(url_in("", 0), None);
        assert_eq!(url_in("https://railway.com", 99), None, "past the end");
    }

    /// The whole point: a link on the emulated screen can be found by where it
    /// is on the screen.
    #[test]
    fn a_link_on_the_screen_is_found_by_position() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 60);
        session.send(b"open https://railway.com/deploy now\r\n");
        // Wait for the whole URL, not just the host. A pty delivers the line in
        // whatever chunks it likes, and "railway.com" is already on screen while
        // the path is still arriving — which left the assertion below comparing
        // against a truncated `…/dep` on a loaded runner.
        for _ in 0..40 {
            if session
                .with_screen(|s| s.contents_between(0, 0, 0, u16::MAX))
                .is_some_and(|line| line.contains("https://railway.com/deploy"))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(
            session.url_at(0, 10).as_deref(),
            Some("https://railway.com/deploy")
        );
        assert_eq!(session.url_at(0, 1), None, "not over the link");
        assert_eq!(session.url_at(99, 10), None, "off the screen");
    }

    /// The case that matters: an OAuth link is longer than the pane is wide, so
    /// it arrives split across rows. Matching within one row finds a fragment
    /// nobody can open.
    #[test]
    fn a_link_wrapped_across_rows_is_found_whole() {
        let url = "https://accounts.example.com/oauth/authorize?client_id=abcdef123456&redirect_uri=http%3A%2F%2Flocalhost%3A8976%2Fcallback&scope=openid+profile";
        assert!(url.len() > 100, "long enough to wrap a 40-column pane");

        // Tall enough that neither the tty's echo of the line nor `cat`'s copy
        // of it pushes the first one off the top — a scrolled-away fragment is
        // a different test.
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(24, 40);
        session.send(format!("{url}\r\n").as_bytes());
        let rows = url.len().div_ceil(40) as u16;
        for _ in 0..100 {
            // The echo, then the copy: waiting for the second guarantees the
            // first is whole.
            if session.url_at(rows, 0).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Every row it covers, and every column within them, resolves to the
        // whole link — clicking the tail is as natural as clicking the head.
        for row in 0..rows {
            let last = if row == rows - 1 {
                (url.len() % 40) as u16
            } else {
                40
            };
            for col in 0..last {
                assert_eq!(
                    session.url_at(row, col).as_deref(),
                    Some(url),
                    "row {row} col {col}"
                );
            }
        }
    }

    /// A wrapped line that is not a link stays not a link.
    #[test]
    fn wrapping_does_not_invent_links() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(8, 20);
        session.send(b"the quick brown fox jumps over the lazy dog\r\n");
        std::thread::sleep(std::time::Duration::from_millis(80));
        for row in 0..3 {
            for col in 0..20 {
                assert_eq!(session.url_at(row, col), None, "row {row} col {col}");
            }
        }
    }

    /// Scrolling has to change what the renderer reads out of the emulator —
    /// the pane draws from `with_screen`, so a scroll that only moves a counter
    /// would look like nothing happening.
    #[test]
    fn scrolling_changes_what_the_screen_shows() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);

        // More lines than the screen holds, so the early ones fall into
        // scrollback. `cat` echoes them back through the pty.
        for i in 0..40 {
            session.send(format!("line-{i}\r\n").as_bytes());
        }
        // Give the reader thread a moment to fold them in.
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let seen = session
                .with_screen(|screen| screen.contents().contains("line-39"))
                .unwrap_or(false);
            if seen {
                break;
            }
        }
        let live = session.with_screen(|s| s.contents()).unwrap();
        assert!(live.contains("line-39"), "expected the tail:\n{live}");
        assert!(!session.scrolled_back());

        session.scroll_by(10);
        assert!(session.scrolled_back(), "the offset should have moved");
        let scrolled = session.with_screen(|s| s.contents()).unwrap();
        assert_ne!(
            scrolled, live,
            "the screen must actually change:\n{scrolled}"
        );

        // Typing returns to the live view.
        session.send(b"x");
        assert!(!session.scrolled_back());
    }

    /// The whole retained history is reachable, not one screenful. The old
    /// emulator could not compose a view deeper than the pane is tall, so a
    /// clamp in `scroll_by` stopped exactly here — this is the regression
    /// test for its removal.
    #[test]
    fn scrolling_reaches_the_whole_history() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);

        for i in 0..120 {
            session.send(format!("line-{i}\r\n").as_bytes());
        }
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let seen = session
                .with_screen(|screen| screen.contents().contains("line-119"))
                .unwrap_or(false);
            if seen {
                break;
            }
        }

        // Ask for infinitely far back; the emulator clamps to what exists.
        session.scroll_by(isize::MAX);
        assert!(
            session.scroll > 100,
            "120 lines through a 6-row pane should leave far more than one \
             screen of history, got offset {}",
            session.scroll
        );
        let top = session.with_screen(|s| s.contents()).unwrap();
        assert!(
            top.contains("line-0"),
            "the very first line should be visible at full depth:\n{top}"
        );

        // And all the way forward again.
        session.scroll_by(isize::MIN);
        assert!(!session.scrolled_back());
        let live = session.with_screen(|s| s.contents()).unwrap();
        assert!(live.contains("line-119"), "back to the tail:\n{live}");
    }

    /// Successive wheel notches keep going past one screenful, through the
    /// same entry point the mouse uses.
    #[test]
    fn scrolling_walks_past_one_screenful() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);

        for i in 0..60 {
            session.send(format!("line-{i}\r\n").as_bytes());
        }
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let seen = session
                .with_screen(|screen| screen.contents().contains("line-59"))
                .unwrap_or(false);
            if seen {
                break;
            }
        }

        // No mouse reporting and no alternate screen here, so each wheel goes
        // to the emulator's own scrollback.
        session.scroll(true, 5, (1, 1));
        let one = session.scroll;
        session.scroll(true, 5, (1, 1));
        let two = session.scroll;
        session.scroll(true, 5, (1, 1));
        let three = session.scroll;
        assert!(one < two && two < three, "each notch must go deeper");
        assert!(
            three > 6,
            "three notches should pass the height of the pane, got {three}"
        );

        let deep = session.with_screen(|s| s.contents()).unwrap();
        assert!(
            !deep.contains("line-59"),
            "the tail should have scrolled out of view:\n{deep}"
        );
    }

    /// A deep offset survives the pane changing shape. Resize used to clamp
    /// the offset to the new height because the old emulator would underflow
    /// past it; now the offset just rides along.
    #[test]
    fn a_deep_scroll_survives_resize() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(10, 40);

        for i in 0..100 {
            session.send(format!("line-{i}\r\n").as_bytes());
        }
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let seen = session
                .with_screen(|screen| screen.contents().contains("line-99"))
                .unwrap_or(false);
            if seen {
                break;
            }
        }

        session.scroll_by(60);
        assert!(session.scroll > 10, "start well past one screen");

        // Shrink, then grow. Either way the view must keep rendering — in
        // debug builds an underflow inside the emulator would panic here.
        session.resize(4, 40);
        assert!(session.scrolled_back(), "the offset survives shrinking");
        let shrunk = session.with_screen(|s| s.contents()).unwrap();
        assert!(!shrunk.is_empty(), "a shrunk pane still renders history");

        session.resize(20, 40);
        let grown = session.with_screen(|s| s.contents()).unwrap();
        assert!(!grown.is_empty(), "a grown pane still renders history");

        // Typing is still the way back to live.
        session.send(b"x");
        assert!(!session.scrolled_back());
    }

    /// Scrolling, resizing, and live output all at once. None of these
    /// operations may wedge the offset, wedge each other, or leave the view
    /// unable to render — the wheel arrives whenever it arrives, not when the
    /// pane is conveniently idle.
    #[test]
    fn scrollback_survives_churn() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(8, 40);

        // Interleave output with scrolls and reshapes, deterministically.
        let sizes = [(4u16, 30u16), (12, 60), (6, 40), (24, 80), (8, 40)];
        for (round, &(rows, cols)) in sizes.iter().enumerate() {
            for i in 0..40 {
                session.send(format!("round-{round}-line-{i}\r\n").as_bytes());
            }
            session.scroll_by(37);
            session.resize(rows, cols);
            session.scroll_by(-13);
            let held = session.scroll;
            let history = session
                .with_screen(|s| s.scrollback())
                .expect("the emulator stays lockable");
            assert_eq!(held, history, "the held offset tracks the emulator");
            assert!(
                session.with_screen(|s| s.contents()).is_some(),
                "the view renders mid-churn"
            );
        }

        // Wait for the tail so the final checks see settled history.
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let seen = session
                .with_screen(|screen| screen.contents().contains("round-4-line-39"))
                .unwrap_or(false);
            if seen {
                break;
            }
        }

        session.scroll_by(isize::MAX);
        let top = session.with_screen(|s| s.contents()).unwrap();
        assert!(
            top.contains("round-0-line-"),
            "the first round is still reachable at full depth:\n{top}"
        );
        session.send(b"x");
        assert!(!session.scrolled_back(), "typing still snaps back to live");
    }

    /// Past the emulator's retention the offset clamps to what is kept, and
    /// the oldest lines are the ones to go — the view at full depth is the
    /// start of the *retained* history, never garbage.
    #[test]
    fn scrollback_clamps_at_capacity() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);

        // More than the 4000 lines the parser retains.
        for i in 0..4200 {
            session.send(format!("line-{i}\r\n").as_bytes());
        }
        for _ in 0..300 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let seen = session
                .with_screen(|screen| screen.contents().contains("line-4199"))
                .unwrap_or(false);
            if seen {
                break;
            }
        }

        session.scroll_by(isize::MAX);
        assert_eq!(
            session.scroll, 4000,
            "full depth is the retention limit, no further"
        );
        let top = session.with_screen(|s| s.contents()).unwrap();
        assert!(
            !top.contains("line-0\r") && !top.contains("line-0\n"),
            "the very first lines fell out of retention:\n{top}"
        );
        assert!(
            top.contains("line-"),
            "what is shown is still real history:\n{top}"
        );
    }

    /// An application that asked for mouse reporting gets a real wheel event,
    /// so its own viewport scrolls. Arrow keys were wrong here: a coding agent
    /// reads those as prompt history, so the wheel walked through old prompts.
    /// Unix only: this needs a mode-setting escape sequence to survive the trip
    /// through the pty, and Windows' ConPTY interprets those for itself instead
    /// of passing them along, so the emulator never sees the mode change. Plain
    /// text round-trips fine, which is why the rest of these run everywhere.
    #[cfg(unix)]
    #[test]
    fn scrolling_an_alternate_screen_reaches_the_application() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);
        // Enter the alternate screen the way an application does. The newline
        // matters: the pty is line-buffered, so `cat` holds anything without
        // one and the emulator never sees the sequence.
        session.send(b"\x1b[?1049h\r\n");
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if session
                .with_screen(|s| s.alternate_screen())
                .unwrap_or(false)
            {
                break;
            }
        }
        assert!(
            session.with_screen(|s| s.alternate_screen()).unwrap(),
            "the fixture should be on the alternate screen"
        );

        // `cat` echoes whatever we send it, so the arrows come back as input.
        session.scroll(true, 2, (1, 1));
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if session.scrolled_back() {
                break;
            }
        }
        assert!(
            !session.scrolled_back(),
            "an alternate screen must not scroll locally"
        );
    }

    /// The wheel report itself: buttons 64 and 65, in whichever encoding the
    /// application asked for.
    #[test]
    fn wheel_reports_match_the_terminal_protocol() {
        let sgr_up = wheel_report(true, (12, 5), vt100::MouseProtocolEncoding::Sgr);
        assert_eq!(String::from_utf8(sgr_up).unwrap(), "\x1b[<64;12;5M");
        let sgr_down = wheel_report(false, (1, 1), vt100::MouseProtocolEncoding::Sgr);
        assert_eq!(String::from_utf8(sgr_down).unwrap(), "\x1b[<65;1;1M");

        // The legacy encoding offsets by 32 and cannot express a big column,
        // so it clamps rather than wrapping into nonsense.
        let legacy = wheel_report(true, (300, 2), vt100::MouseProtocolEncoding::Default);
        assert_eq!(legacy[..3], [0x1b, b'[', b'M']);
        assert_eq!(legacy[3], 96, "button 64 plus the 32 offset");
        assert_eq!(legacy[4], 255, "clamped to the encodable maximum");
        assert_eq!(legacy[5], 34);
    }

    /// The reply the emulator would send back for a cursor-position query —
    /// 1-indexed, and read off wherever the chunk that carried the query
    /// itself left the cursor.
    #[test]
    fn dsr_reply_answers_with_the_current_cursor_position() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"hello\r\n\x1b[6n");
        let reply = dsr_reply(b"hello\r\n\x1b[6n", parser.screen());
        assert_eq!(reply, Some(b"\x1b[2;1R".to_vec()));
    }

    /// Ordinary output — the vast majority of what comes through — is not a
    /// query, and must not be answered as though it were one.
    #[test]
    fn dsr_reply_is_none_without_a_query() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"just some output\r\n");
        assert_eq!(dsr_reply(b"just some output\r\n", parser.screen()), None);
    }

    /// An application with mouse reporting on gets the wheel; the emulator's
    /// own scrollback stays where it was.
    /// Unix only: this needs a mode-setting escape sequence to survive the trip
    /// through the pty, and Windows' ConPTY interprets those for itself instead
    /// of passing them along, so the emulator never sees the mode change. Plain
    /// text round-trips fine, which is why the rest of these run everywhere.
    #[cfg(unix)]
    #[test]
    fn a_mouse_aware_application_receives_the_wheel() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);
        // Alternate screen plus SGR mouse reporting: what a coding agent sets.
        session.send(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\r\n");
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let ready = session
                .with_screen(|s| {
                    s.alternate_screen()
                        && s.mouse_protocol_mode() != vt100::MouseProtocolMode::None
                })
                .unwrap_or(false);
            if ready {
                break;
            }
        }
        assert!(
            session
                .with_screen(|s| s.mouse_protocol_mode() != vt100::MouseProtocolMode::None)
                .unwrap(),
            "the fixture should have mouse reporting on"
        );
        assert!(session.scrollable(), "the wheel has somewhere to go");

        session.scroll(true, 3, (4, 2));
        assert!(
            !session.scrolled_back(),
            "the wheel went to the application, not to local history"
        );
    }

    #[test]
    fn pointer_reports_match_the_terminal_protocol() {
        use vt100::MouseProtocolEncoding::{Default as Legacy, Sgr};

        let press = pointer_report(Pointer::Press, (12, 5), Sgr);
        assert_eq!(String::from_utf8(press).unwrap(), "\x1b[<0;12;5M");
        let drag = pointer_report(Pointer::Drag, (12, 6), Sgr);
        assert_eq!(String::from_utf8(drag).unwrap(), "\x1b[<32;12;6M");
        // SGR marks a release with a lowercase final byte, which is the whole
        // reason applications ask for it.
        let release = pointer_report(Pointer::Release, (12, 6), Sgr);
        assert_eq!(String::from_utf8(release).unwrap(), "\x1b[<0;12;6m");

        // The legacy encoding has no separate release, so it is button 3.
        let legacy = pointer_report(Pointer::Release, (2, 3), Legacy);
        assert_eq!(legacy, vec![0x1b, b'[', b'M', 32 + 3, 34, 35]);
    }

    /// The click that makes "click here to copy" work: the application is
    /// listening, so the event goes to it.
    /// Unix only: this needs a mode-setting escape sequence to survive the trip
    /// through the pty, and Windows' ConPTY interprets those for itself instead
    /// of passing them along, so the emulator never sees the mode change. Plain
    /// text round-trips fine, which is why the rest of these run everywhere.
    #[cfg(unix)]
    #[test]
    fn a_mouse_aware_application_receives_a_click() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);
        session.send(b"\x1b[?1002h\x1b[?1006h\r\n");
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if session.wants_mouse() {
                break;
            }
        }
        assert!(session.wants_mouse(), "the fixture should want the mouse");

        assert!(session.pointer(Pointer::Press, (4, 2)));
        assert!(session.pointer(Pointer::Drag, (6, 2)));
        assert!(session.pointer(Pointer::Release, (6, 2)));
    }

    /// An application that never asked keeps its clicks: the pane's own
    /// selection and link handling stay in charge.
    #[test]
    fn an_application_without_mouse_reporting_gets_no_clicks() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!session.wants_mouse());
        assert!(!session.pointer(Pointer::Press, (4, 2)));
        assert!(!session.pointer(Pointer::Release, (4, 2)));
    }

    /// Press-only mode is exactly that. Sending it motion would be reporting
    /// something it never asked to hear about.
    /// Unix only: this needs a mode-setting escape sequence to survive the trip
    /// through the pty, and Windows' ConPTY interprets those for itself instead
    /// of passing them along, so the emulator never sees the mode change. Plain
    /// text round-trips fine, which is why the rest of these run everywhere.
    #[cfg(unix)]
    #[test]
    fn press_only_mode_hears_only_presses() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);
        session.send(b"\x1b[?9h\r\n");
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if session.wants_mouse() {
                break;
            }
        }
        assert!(session.wants_mouse());

        assert!(session.pointer(Pointer::Press, (4, 2)));
        assert!(!session.pointer(Pointer::Drag, (5, 2)));
        assert!(!session.pointer(Pointer::Release, (5, 2)));
    }

    /// The emulator underflows if the offset passes the screen height, so the
    /// clamp is load-bearing rather than tidiness — without it a big scroll is
    /// a panic in debug and a silent no-op in release.
    #[test]
    fn scrolling_cannot_pass_the_emulators_limit() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(6, 40);
        for i in 0..40 {
            session.send(format!("line-{i}\r\n").as_bytes());
        }
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if session
                .with_screen(|s| s.contents().contains("line-39"))
                .unwrap_or(false)
            {
                break;
            }
        }

        // Far past the ceiling; the screen still renders.
        session.scroll_by(10_000);
        let contents = session.with_screen(|s| s.contents());
        assert!(contents.is_some(), "the screen must still be readable");

        // Shrinking the pane must bring the offset down with it.
        session.resize(3, 40);
        let contents = session.with_screen(|s| s.contents());
        assert!(contents.is_some(), "a shrink must not leave a bad offset");

        session.scroll_by(-10_000);
        assert!(!session.scrolled_back(), "and back to live");
    }

    /// The emulator is the contract with the renderer: bytes in, a screen we
    /// can read out. Exercised without a pty so it runs anywhere.
    #[test]
    fn the_emulator_renders_what_was_written() {
        let mut parser = vt100::Parser::new(4, 20, 100);
        parser.process(b"hello\r\nworld");
        let screen = parser.screen();
        assert_eq!(screen.contents().lines().next().unwrap().trim(), "hello");
        assert!(screen.contents().contains("world"));
    }
}
