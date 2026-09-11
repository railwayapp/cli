//! A live agent session rendered inside the TUI.
//!
//! SSH or a local remote-server client runs under a pty we own; its output is fed to a host-side terminal
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

use crate::commands::cloud_agent::{client_sessions, codex};
use crate::commands::ssh::native;
use crate::vt100;

use super::terminal_palette;

type PaneParser = vt100::Parser<PaletteReplies>;

fn pane_parser(rows: u16, cols: u16, scrollback: usize) -> PaneParser {
    PaneParser::new_with_callbacks(
        rows,
        cols,
        scrollback,
        PaletteReplies {
            colors: terminal_palette::cached(),
            replies: Vec::new(),
        },
    )
}

/// Let the emulator parse OSC, including BEL/ST terminators and split reads.
/// Only answer palette queries; color setters and other OSCs stay pane-local.
struct PaletteReplies {
    colors: Option<terminal_palette::DefaultColors>,
    replies: Vec<u8>,
}

impl vt100::Callbacks for PaletteReplies {
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        let Some(colors) = &self.colors else {
            // An unknown host palette must stay unknown: inventing a dark
            // background makes Codex's shaded controls illegible on light themes.
            return;
        };
        let (code, color) = match params {
            [b"10", b"?"] => (10, &colors.fg),
            [b"11", b"?"] => (11, &colors.bg),
            _ => return,
        };
        self.replies.extend_from_slice(
            format!(
                "\x1b]{code};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
                color.r, color.g, color.b
            )
            .as_bytes(),
        );
    }
}

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

/// The reply to a primary-device-attributes query (`ESC[c`, or `ESC[0c` with
/// the parameter spelled out), found anywhere in a chunk — `Some` iff the
/// query is there.
///
/// The other query that blocks the program which sent it, and the one that
/// stopped `railway-agent-tui` drawing at all. crossterm uses DA1 as the
/// sentinel in `supports_keyboard_enhancement`: it writes the kitty query and
/// a DA1 immediately after, then reads until one of them comes back, on the
/// reasoning that a terminal too old to know the kitty query will still answer
/// DA1. So answering the kitty query while ignoring DA1 is the single worst
/// combination available — the harness learns the reply it is waiting for will
/// never arrive, and waits anyway. That is exactly what this pane started
/// doing when it learned to answer `ESC[?u`: the launch went from drawing
/// after a two-second timeout to never drawing at all, leaving a pane with
/// nothing in it but the relay's banner. Answering both retires the timeout
/// too — startup goes from ~2s to immediate.
///
/// `62;22` claims a VT220 that does ANSI colour, which is the least this
/// emulator is. Secondary DA (`ESC[>c`) is deliberately not answered: nothing
/// here asks for it, and inventing a version string for a terminal that does
/// not exist invites feature detection nobody can honour.
fn da1_reply(chunk: &[u8]) -> Option<Vec<u8>> {
    const REPLY: &[u8] = b"\x1b[?62;22c";
    let mut i = 0;
    while let Some(at) = chunk[i..].windows(2).position(|w| w == b"\x1b[") {
        let seq = &chunk[i + at + 2..];
        // A query carries no parameters or the single default `0`. Anything
        // else ending in `c` is a different sequence — and `ESC[?…c` is a
        // terminal's own reply, never a request, so a leading `?` is not one.
        let query = match seq.first() {
            Some(b'c') => true,
            Some(b'0') => seq.get(1) == Some(&b'c'),
            _ => false,
        };
        if query {
            return Some(REPLY.to_vec());
        }
        // Step past the introducer only, like `kitty_scan`: a later `c` may
        // belong to plain text, and skipping to it would jump real sequences.
        i += at + 2;
    }
    None
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
/// [`TerminalReplies`] supplies complete sequences, including those split
/// across SSH reads.
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

/// Answer terminal queries as a stream: SSH can split a query at any byte,
/// and a harness may wait for its answer without writing anything else.
#[derive(Default)]
struct TerminalReplies {
    pending: Vec<u8>,
}

impl TerminalReplies {
    fn process(&mut self, bytes: &[u8], parser: &mut PaneParser, kitty: &AtomicBool) -> Vec<u8> {
        let mut replies = Vec::new();
        let mut parsed = 0;
        for (i, &byte) in bytes.iter().enumerate() {
            if byte == 0x1b {
                self.pending.clear();
                self.pending.push(byte);
            } else if self.pending == b"\x1b" {
                if byte == b'[' {
                    self.pending.push(byte);
                } else {
                    self.pending.clear();
                }
            } else if !self.pending.is_empty() {
                self.pending.push(byte);
                if (0x40..=0x7e).contains(&byte) {
                    // Process up to this query before answering, so multiple
                    // cursor queries in one read each see their own position.
                    parser.process(&bytes[parsed..=i]);
                    parsed = i + 1;
                    // OSC palette queries before this CSI must be answered
                    // before DA1, which Codex uses as its probe's sentinel.
                    replies.append(&mut parser.callbacks_mut().replies);
                    if let Some(reply) = kitty_scan(&self.pending, kitty) {
                        replies.extend(reply);
                    }
                    if let Some(reply) = da1_reply(&self.pending) {
                        replies.extend(reply);
                    }
                    if let Some(reply) = dsr_reply(&self.pending, parser.screen()) {
                        replies.extend(reply);
                    }
                    self.pending.clear();
                } else if !(0x20..=0x3f).contains(&byte) || self.pending.len() > 64 {
                    self.pending.clear();
                }
            }
        }
        parser.process(&bytes[parsed..]);
        replies.append(&mut parser.callbacks_mut().replies);
        replies
    }
}

/// The relay announces the durable session on connect
/// (`Railway durable session: <name>`, see `sandbox ssh --session`'s docs).
/// That line lands in this pane's pty before the harness draws, and without
/// filtering it keeps the emulator's first row — the harness clears what it
/// drew, not what arrived before it, so the announcement sits on top of the
/// agent's TUI for the whole session.
// The relay has shipped the announcement both with and without a colon.
const DURABLE_BANNER_MARKER: &[u8] = b"Railway durable session";
/// Give up looking after this much input: the relay prints immediately, so
/// anything later containing the marker is the session's own output, not the
/// announcement, and must be kept.
const BANNER_GIVE_UP_AFTER: usize = 32 * 1024;

/// Streaming filter for the relay's durable-session announcement.
///
/// Chunk-wise like [`kitty_scan`]: complete banner lines are removed, an
/// incomplete tail is held for the next `push`, and `flush` drains it at EOF.
/// Only the first announcement is removed — afterwards (or after enough
/// banner-free input) the filter is done so identical text the user or the
/// harness prints later is preserved.
struct BannerFilter {
    pending: Vec<u8>,
    seen: usize,
    done: bool,
    name: Option<String>,
}

impl BannerFilter {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            seen: 0,
            done: false,
            name: None,
        }
    }

    fn find_marker(haystack: &[u8]) -> Option<usize> {
        if haystack.len() < DURABLE_BANNER_MARKER.len() {
            return None;
        }
        haystack
            .windows(DURABLE_BANNER_MARKER.len())
            .position(|w| w == DURABLE_BANNER_MARKER)
    }

    /// Remove every *complete* banner line in `data` (a line with a
    /// terminator). A marker with no terminator yet is left for the next
    /// chunk. Returns whether anything was removed.
    fn strip_complete_lines(&mut self, data: &mut Vec<u8>) -> bool {
        let mut removed = false;
        while let Some(pos) = Self::find_marker(data) {
            let rest = &data[pos..];
            let end_rel = rest
                .iter()
                .position(|b| *b == b'\n')
                .or_else(|| rest.iter().position(|b| *b == b'\r'));
            let Some(end_rel) = end_rel else {
                // No terminator yet — wait for more input.
                break;
            };
            let end = pos + end_rel + 1;
            if self.name.is_none() {
                let value = String::from_utf8_lossy(&rest[DURABLE_BANNER_MARKER.len()..end_rel]);
                if let Some(name) = value
                    .trim_start_matches([' ', ':'])
                    .split_whitespace()
                    .next()
                    .map(|name| name.trim_end_matches('.'))
                    && client_sessions::validate_id(name).is_ok()
                {
                    self.name = Some(name.into());
                }
            }
            // A leading blank line that only exists to carry the banner
            // (the pty starts with `\r\n` before the announcement) goes with
            // it, or the pane keeps a blank first row instead of the banner.
            let start = if data[..pos].iter().all(|b| *b == b'\r' || *b == b'\n') {
                0
            } else {
                pos
            };
            data.drain(start..end);
            removed = true;
        }
        removed
    }

    /// Feed a read through the filter; the returned bytes are what the
    /// emulator should see (possibly empty when only banner arrived).
    fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.done {
            return chunk.to_vec();
        }
        self.seen += chunk.len();
        let mut data = std::mem::take(&mut self.pending);
        data.extend_from_slice(chunk);
        if self.strip_complete_lines(&mut data) {
            self.done = true;
            return data;
        }
        if self.seen >= BANNER_GIVE_UP_AFTER {
            self.done = true;
            return data;
        }
        // Hold only a possible banner, never an arbitrary tail of terminal
        // output. A cursor query can be the last thing a harness writes until
        // we answer it; buffering that query deadlocks terminal startup when
        // the relay's banner is absent or has changed format.
        let split = if let Some(pos) = Self::find_marker(&data) {
            pos
        } else {
            let held = (1..DURABLE_BANNER_MARKER.len())
                .rev()
                .find(|&len| data.ends_with(&DURABLE_BANNER_MARKER[..len]))
                .unwrap_or(0);
            data.len() - held
        };
        self.pending = data.split_off(split);
        data
    }

    /// Drain at EOF, removing even an unterminated trailing banner.
    fn flush(&mut self) -> Vec<u8> {
        if self.done {
            return std::mem::take(&mut self.pending);
        }
        self.done = true;
        let mut data = std::mem::take(&mut self.pending);
        while let Some(pos) = Self::find_marker(&data) {
            let start = if data[..pos].iter().all(|b| *b == b'\r' || *b == b'\n') {
                0
            } else {
                pos
            };
            data.drain(start..);
        }
        data
    }
}

/// A running `ssh` under a pty, plus the emulator that makes sense of it.
pub struct Session {
    pub agent_id: String,
    pub agent_name: String,
    /// The harness this session was started with, when we started it — what a
    /// respawn after an unasked-for exit launches again.
    pub harness: String,
    /// The durable session this pane is attached to.
    pub durable_name: String,
    /// SSH transport identity for a pane whose visible identity is a thread.
    pub console_name: Option<String>,
    announced_console: Arc<Mutex<Option<String>>>,
    pub client_id: Option<String>,
    pub client_thread: Option<client_sessions::Thread>,
    pub client_bridge: Option<codex::bridge::Bridge>,
    pub opencode_bridge: Option<crate::commands::cloud_agent::opencode::bridge::Bridge>,
    /// How this pane connected, kept so the same session can be reopened
    /// full-screen without rebuilding the relay plumbing.
    pub ssh_target: String,
    pub identity: Option<std::path::PathBuf>,
    pub relay_opts: Vec<String>,
    parser: Arc<Mutex<PaneParser>>,
    /// Shared with the reader thread, which also writes to it — a synthetic
    /// cursor-position reply (see [`dsr_reply`]) has to go back over the same
    /// pty the keyboard does, and `take_writer` can only be called once.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// Set by the reader thread when ssh's output ends — the session is over
    /// even though the child may take another moment to reap.
    ended: Arc<AtomicBool>,
    /// Whether ssh exited cleanly, once its status has been collected — the
    /// difference between "the harness finished" (0: the pane can close) and
    /// "the connection dropped" (anything else: the pane stays for the
    /// recovery keys). `None` until waitpid has it; see
    /// [`Self::exit_success`], which fills this exactly once.
    exit_status: Option<bool>,
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
    /// When local input last reached the pane. A deliberate exit — ctrl-c,
    /// ctrl-d, `/exit` — arrives as keystrokes moments before ssh ends, and
    /// this is what lets the reap tell "the user ended it" from "it died".
    last_input: Option<std::time::Instant>,
    /// Last size pushed to the pty, so a redraw at the same size is free.
    size: (u16, u16),
    /// Rows scrolled back from the live view. Typing snaps back to 0 — nobody
    /// wants to type into history.
    scroll: usize,
}

impl Session {
    /// The relay can replace a requested name with its own durable petname.
    /// Keep that exact transport identity separate from the visible thread.
    pub(super) fn sync_console_name(&mut self) {
        if self.client_bridge.is_none()
            && self.opencode_bridge.is_none()
            && let Some(name) = self
                .announced_console
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        {
            self.console_name = Some(name);
        }
    }

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
        Self::spawn_pty(
            agent_id,
            agent_name,
            harness,
            ssh_target,
            identity,
            relay_opts,
            reattach,
            durable_session,
            cmd,
            rows,
            cols,
            notify,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_client(
        agent_id: String,
        agent_name: String,
        binary: &std::path::Path,
        connection: &client_sessions::Connection,
        client_url: Option<&str>,
        thread_id: Option<&str>,
        prompt: Option<&str>,
        rows: u16,
        cols: u16,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let mut cmd = CommandBuilder::new(binary);
        let mut local_connection = connection.clone();
        if let Some(url) = client_url {
            match &mut local_connection {
                client_sessions::Connection::Codex(c) => c.url = url.into(),
                client_sessions::Connection::OpenCode(c, _) => c.url = url.into(),
            }
        }
        cmd.args(local_connection.args(thread_id));
        if let Some(prompt) = prompt {
            match connection {
                client_sessions::Connection::Codex(_) => {
                    cmd.args(["--", prompt]);
                }
                client_sessions::Connection::OpenCode(_, true) => {
                    cmd.args(["--prompt", prompt]);
                }
                _ => {}
            }
        }
        match connection {
            client_sessions::Connection::Codex(c) => {
                cmd.env(codex::TOKEN_ENV, &c.token);
                cmd.env("CODEX_HOME", codex::local::client_home(c)?);
            }
            client_sessions::Connection::OpenCode(c, _) => {
                cmd.env("OPENCODE_SERVER_USERNAME", &c.username);
                cmd.env("OPENCODE_SERVER_PASSWORD", &c.password);
            }
        }
        let name = client_sessions::name(connection.harness(), &agent_id, thread_id);
        Self::spawn_pty(
            agent_id,
            agent_name,
            connection.harness().into(),
            "",
            None,
            &[],
            false,
            &name,
            cmd,
            rows,
            cols,
            notify,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_pty(
        agent_id: String,
        agent_name: String,
        harness: String,
        ssh_target: &str,
        identity: Option<&std::path::Path>,
        relay_opts: &[String],
        reattach: bool,
        durable_session: &str,
        mut cmd: CommandBuilder,
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
        // The emulator understands xterm sequences and the relay does not
        // forward COLORTERM, so both are stated here rather than inherited.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pty
            .slave
            .spawn_command(cmd)
            .context("Failed to start the agent client")?;
        // The slave handle must go before the reader starts, or the pty never
        // reports EOF when ssh exits and the reader thread parks forever.
        drop(pty.slave);

        let parser = Arc::new(Mutex::new(pane_parser(rows, cols, 4000)));
        let ended = Arc::new(AtomicBool::new(false));
        let got_output = Arc::new(AtomicBool::new(false));
        let kitty_keys = Arc::new(AtomicBool::new(false));
        let announced_console = Arc::new(Mutex::new(None));
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
            let announced_console = announced_console.clone();
            std::thread::spawn(move || {
                // 64K per read, not 8K: a reattach replays the session's
                // recorded output in one burst, and this thread is the only
                // thing draining the pty. Fall behind and the far side backs
                // up — ssh's keepalive replies queue behind the flood, and
                // after ServerAliveCountMax of them go missing ssh kills the
                // connection mid-replay. Fewer, larger reads keep the drain
                // ahead of the network.
                let mut buf = [0u8; 65536];
                let mut banner = BannerFilter::new();
                let mut terminal_replies = TerminalReplies::default();
                // Fold filtered bytes into the emulator plus the terminal
                // queries it may carry. Banner-only reads feed nothing, so
                // they neither count as session output (a dead attach that
                // only ever sent the announcement must still read as stalled)
                // nor cause a redraw.
                let mut feed = |bytes: &[u8],
                                parser: &Arc<Mutex<PaneParser>>,
                                kitty_keys: &Arc<AtomicBool>,
                                writer: &Arc<Mutex<Box<dyn Write + Send>>>,
                                got_output: &Arc<AtomicBool>,
                                notify: &dyn Fn()| {
                    if bytes.is_empty() {
                        return;
                    }
                    got_output.store(true, Ordering::Relaxed);
                    let replies = parser
                        .lock()
                        .map(|mut parser| terminal_replies.process(bytes, &mut parser, kitty_keys))
                        .unwrap_or_default();
                    if !replies.is_empty() {
                        if let Ok(mut writer) = writer.lock() {
                            let _ = writer.write_all(&replies);
                            let _ = writer.flush();
                        }
                    }
                    notify();
                };
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let filtered = banner.push(&buf[..n]);
                            if let Some(name) = banner.name.take() {
                                *announced_console.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Some(name);
                                notify();
                            }
                            feed(
                                &filtered,
                                &parser,
                                &kitty_keys,
                                &writer,
                                &got_output,
                                &notify,
                            );
                        }
                    }
                }
                let tail = banner.flush();
                feed(&tail, &parser, &kitty_keys, &writer, &got_output, &notify);
                ended.store(true, Ordering::Relaxed);
                notify();
            });
        }

        Ok(Self {
            announced_console,
            console_name: None,
            client_id: None,
            client_thread: None,
            client_bridge: None,
            opencode_bridge: None,
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
            exit_status: None,
            reattach,
            spawned_at: std::time::Instant::now(),
            got_output,
            kitty_keys,
            last_input: None,
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

    /// How long this pane has been open. The tree uses it to tell the status
    /// projection's normal lag apart from an agent that is genuinely stuck —
    /// see `displayed_status`.
    pub fn open_for(&self) -> std::time::Duration {
        self.spawned_at.elapsed()
    }

    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }

    /// The remote command ran to completion: ssh exited 0, which a pane-style
    /// launch only does once the harness has exited and the reset has run.
    /// This is "the work here is over" — the pane can close on it. A dropped
    /// connection — relay death, an idle NAT timeout, a kill — exits nonzero
    /// instead and is *not* finished: that pane stays up for the recovery
    /// keys, because the durable session it was showing is still running.
    pub fn finished(&mut self) -> bool {
        self.ended() && self.exit_success() == Some(true)
    }

    /// The connection died under the session rather than finishing: ssh ended
    /// without a clean exit. The recoverable state — the durable session is
    /// almost certainly still running on the agent, and `r` dials it again.
    pub fn dropped(&mut self) -> bool {
        self.ended() && self.exit_success() == Some(false)
    }

    /// Ended, but ssh's exit status hasn't been collected yet — the pty's EOF
    /// can beat waitpid by a beat. The event loop polls briefly while any
    /// pane is in this state, so the finished/dropped call isn't lost to the
    /// race.
    pub fn awaiting_exit_status(&self) -> bool {
        self.ended() && self.exit_status.is_none()
    }

    /// ssh's exit, collected once and remembered. `None` while the status
    /// isn't available yet; a wait that errors counts as "not clean", which
    /// keeps the pane — the conservative wrong answer.
    fn exit_success(&mut self) -> Option<bool> {
        if self.exit_status.is_none() {
            self.exit_status = match self.child.try_wait() {
                Ok(None) => None,
                Ok(Some(status)) => Some(status.success()),
                Err(_) => Some(false),
            };
        }
        self.exit_status
    }

    /// The environment this session's agent lives in, read back out of the
    /// relay target (`agent:<environment>:<agent>`) it connected with.
    ///
    /// Reconnecting after a drop needs it — `connect_info` resolves the relay
    /// plumbing from environment and agent — and the target is the one place
    /// the session still carries it.
    pub fn environment_id(&self) -> Option<String> {
        let mut parts = self.ssh_target.split(':');
        match (parts.next()?, parts.next()) {
            ("agent", Some(env)) if !env.is_empty() => Some(env.to_string()),
            _ => None,
        }
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
        self.last_input = Some(std::time::Instant::now());
        self.write_raw(bytes);
    }

    /// Local input reached the pane within `window`. Every deliberate way out
    /// of a harness — ctrl-c, ctrl-d, typing `/exit` — is keystrokes moments
    /// before ssh ends, so a finish with no recent input is one the user did
    /// not ask for.
    pub fn input_within(&self, window: std::time::Duration) -> bool {
        self.last_input.is_some_and(|at| at.elapsed() < window)
    }

    /// How long this pane has been connected.
    pub fn age(&self) -> std::time::Duration {
        self.spawned_at.elapsed()
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        if let Some(bytes) = encode_key_for(key, self.kitty_keys.load(Ordering::Relaxed)) {
            self.send(&bytes);
        }
    }

    /// Forward pasted text as a paste, not as typed keys. When the program in
    /// the pane has bracketed paste switched on (Claude Code and every modern
    /// editor do), the text goes wrapped in the paste markers and lands as one
    /// atomic paste — a newline stays a newline instead of hitting Enter. A
    /// program that never asked for the mode (a plain shell) gets the bare
    /// text, exactly what a real terminal would send it.
    pub fn send_paste(&mut self, text: &str) {
        let bracketed = self.with_screen(|s| s.bracketed_paste()).unwrap_or(false);
        self.send(&encode_paste(text, bracketed));
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
    /// Put the session in the state [`Self::finished`] looks for: reader
    /// done, ssh exited clean.
    pub fn end_for_test(&mut self) {
        self.ended.store(true, Ordering::Relaxed);
        self.exit_status = Some(true);
    }

    /// Ended with a nonzero exit — the shape of a dropped connection.
    pub fn end_dropped_for_test(&mut self) {
        self.ended.store(true, Ordering::Relaxed);
        self.exit_status = Some(false);
    }

    /// Flip the session to ended without waiting for its process to die —
    /// the reader thread races a test that killed `cat`, and the state under
    /// test is "the connection is gone", not how it went.
    pub fn mark_ended(&self) {
        self.ended.store(true, Ordering::Relaxed);
    }

    /// Pretend the user just typed into the pane.
    pub fn touch_input_for_test(&mut self) {
        self.last_input = Some(std::time::Instant::now());
    }

    /// Pretend the pane connected `by` ago, for the fast-crash guard.
    pub fn backdate_spawn_for_test(&mut self, by: std::time::Duration) {
        self.spawned_at = std::time::Instant::now() - by;
    }

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
        let parser = Arc::new(Mutex::new(pane_parser(24, 80, 4000)));
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
            console_name: None,
            announced_console: Arc::new(Mutex::new(None)),
            client_id: None,
            client_thread: None,
            client_bridge: None,
            opencode_bridge: None,
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
            exit_status: None,
            reattach: false,
            spawned_at: std::time::Instant::now(),
            got_output: Arc::new(AtomicBool::new(true)),
            kitty_keys: Arc::new(AtomicBool::new(false)),
            last_input: None,
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
    // Without the push the CSI-u form would land as typed text, so ⇧enter falls
    // back to meta+enter — `ESC CR`, the newline chord harnesses have always
    // taken, and the exact bytes Claude Code's own `/terminal-setup` binds
    // shift+enter to. A bare `\r` submits the half-written prompt, the one
    // outcome the chord exists to prevent, so guessing newline is the better
    // way to be wrong. ⌥enter already encodes this way through `encode_key`'s
    // Alt prefix; this puts ⇧enter alongside it.
    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT) {
        return Some(b"\x1b\r".to_vec());
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

/// Encode pasted text as the bytes a terminal would send — wrapped in the
/// bracketed-paste markers when the program on the pty has the mode on, bare
/// otherwise.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    // A paste containing the end marker would terminate the paste early and
    // feed the remainder through as keystrokes — the classic bracketed-paste
    // injection. Real terminals strip it; so does this pane. It is stripped
    // from an unbracketed paste too: that path is keystrokes, and no keyboard
    // produces the sequence.
    let text = text.replace("\x1b[201~", "");
    // Enter arrives from a keyboard as CR, and programs reading a pty expect
    // the same from a paste; LF-only text would land as ^J.
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
        return text.into_bytes();
    }
    let mut bytes = Vec::with_capacity(text.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run with RAILWAY_TEST_CODEX_BIN pointing to a local Codex binary. Uses
    /// only /status in an isolated home; no credentials or model turns.
    #[cfg(unix)]
    #[test]
    #[ignore = "requires an installed Codex binary"]
    fn real_codex_scrollback_survives_panel_focus_and_a_lost_resize_release() {
        use crate::commands::cloud_agent::tui::{
            app::{App, ManageFocus, MouseAction},
            ui,
        };
        use ratatui::{Terminal, backend::TestBackend};
        let binary = std::env::var("RAILWAY_TEST_CODEX_BIN").expect("set RAILWAY_TEST_CODEX_BIN");
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().canonicalize().unwrap();
        std::fs::write(root.path().join("config.toml"), format!(
            "check_for_update_on_startup = false\nmodel_provider = \"scroll_probe\"\nmodel = \"test\"\n[model_providers.scroll_probe]\nname = \"Scroll probe\"\nbase_url = \"http://127.0.0.1:9/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n[projects.{}]\ntrust_level = \"trusted\"\n",
            serde_json::to_string(&directory.to_string_lossy()).unwrap()
        )).unwrap();
        let mut cmd = CommandBuilder::new(binary);
        cmd.env("CODEX_HOME", root.path());
        cmd.arg("-C");
        cmd.arg(&directory);
        let pane = Session::spawn_pty(
            "ca_1".into(),
            "codex-scroll-probe".into(),
            "codex".into(),
            "",
            None,
            &[],
            false,
            "codex-test",
            cmd,
            34,
            102,
            || {},
        )
        .unwrap();
        let mut app = App::new(Vec::new(), None, Some("codex"), None, None, true);
        app.attach_session(pane, "ca_1".into());
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|f| app.panes = ui::render_with_layout(&app, f).0)
            .unwrap();
        let rect = app.panes.session;
        app.sessions[0].resize(rect.h, rect.w);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !app.sessions[0]
            .with_screen(|s| s.contents().contains("test default"))
            .unwrap_or(false)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "Codex startup: {:?}",
                app.sessions[0].last_line()
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Generate enough terminal history without sending work to a model.
        for _ in 0..6 {
            app.sessions[0].send(b"/status");
            std::thread::sleep(std::time::Duration::from_millis(50));
            app.sessions[0].send_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        assert!(!app.sessions[0].wants_mouse());
        assert!(
            !app.sessions[0]
                .with_screen(|s| s.alternate_screen())
                .unwrap()
        );
        let live = app.sessions[0].with_screen(|s| s.contents()).unwrap();
        for _ in 0..8 {
            app.on_mouse(MouseAction::ScrollUp, rect.x + 4, rect.y + 4);
        }
        assert!(
            app.sessions[0].scrolled_back(),
            "Codex history must scroll through the app handler: active={:?}, focus={:?}, screen={live}",
            app.active,
            app.focus
        );
        assert_ne!(app.sessions[0].with_screen(|s| s.contents()).unwrap(), live);
        let offset = app.sessions[0].scroll;
        std::thread::sleep(std::time::Duration::from_millis(200));
        terminal
            .draw(|f| app.panes = ui::render_with_layout(&app, f).0)
            .unwrap();
        assert_eq!(
            app.sessions[0].scroll, offset,
            "redrawing preserves the history view"
        );
        for _ in 0..8 {
            app.on_mouse(MouseAction::ScrollDown, rect.x + 4, rect.y + 4);
        }
        assert!(!app.sessions[0].scrolled_back());
        let divider = app.panes.sidebar_divider;
        app.on_mouse(MouseAction::Down, divider.x, divider.y);
        app.on_mouse(MouseAction::Drag, divider.x + 4, divider.y);
        app.on_mouse(MouseAction::ScrollUp, rect.x + 8, rect.y + 4);
        assert!(app.sessions[0].scrolled_back());
        assert!(!app.resizing_sidebar());
        assert_eq!(app.focus, ManageFocus::Session);
    }

    #[cfg(unix)]
    #[test]
    fn codex_local_pty_handles_remote_args_auth_input_and_resize() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("fake codex");
        std::fs::write(&binary, r#"#!/usr/bin/env python3
import hashlib, json, os, pathlib, sys
assert os.isatty(0) and os.isatty(1)
assert os.environ['RAILWAY_CODEX_SERVER_TOKEN'] == "secret ' $(echo injected)"
backend = hashlib.sha256(b'wss://agent.example.com:443').hexdigest()[:16]
assert pathlib.Path(os.environ['CODEX_HOME']) == pathlib.Path.home() / '.railway/codex-client' / backend
assert sys.argv[1:] == ['-c', 'check_for_update_on_startup=false', '--remote', 'ws://127.0.0.1:54321', '--remote-auth-token-env', 'RAILWAY_CODEX_SERVER_TOKEN', '--cd', '/app/a project', 'resume', 'thread-1']
print('Codex ready', flush=True)
assert input() == 'hello'
size = os.get_terminal_size()
assert (size.lines, size.columns) == (30, 100), size
print('Codex complete', flush=True)
"#).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let connection = codex::Connection {
            url: "wss://agent.example.com:443".into(),
            token: "secret ' $(echo injected)".into(),
            directory: "/app/a project".into(),
            version: "0.153.4".into(),
            reused: true,
        };
        let mut pane = Session::spawn_client(
            "agent-id".into(),
            "box".into(),
            &binary,
            &client_sessions::Connection::Codex(connection),
            Some("ws://127.0.0.1:54321"),
            Some("thread-1"),
            None,
            24,
            80,
            || {},
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !pane
            .with_screen(|s| s.contents().contains("Codex ready"))
            .unwrap_or(false)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "{:?}",
                pane.last_line()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        pane.resize(30, 100);
        pane.write_raw(b"hello\n");
        while !pane.finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "{:?}",
                pane.last_line()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            pane.with_screen(|s| s.contents().contains("Codex complete"))
                .unwrap()
        );
        assert_eq!(
            pane.durable_name,
            client_sessions::name("codex", "agent-id", Some("thread-1"))
        );
        assert!(pane.relay_opts.is_empty());
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[cfg(unix)]
    #[test]
    fn both_opencode_clients_resume_the_requested_thread_inside_a_resizable_pty() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("fake opencode");
        std::fs::write(
            &binary,
            r#"#!/usr/bin/env python3
import os, sys
assert os.isatty(0) and os.isatty(1)
assert os.environ['OPENCODE_SERVER_USERNAME'] == 'opencode'
assert os.environ['OPENCODE_SERVER_PASSWORD'] == "secret ' $(echo injected)"
assert sys.argv[-2:] == ['--session', 'ses_thread1']
if sys.argv[1] == 'attach':
    assert sys.argv[2:-2] == ['https://agent.example.com', '--dir', '/app/a project']
else:
    assert sys.argv[1:-2] == ['--server', 'https://agent.example.com', '--auto']
print('ready', flush=True)
assert input() == 'hello'
size = os.get_terminal_size()
assert (size.lines, size.columns) == (30, 100)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        for beta in [false, true] {
            let c = crate::commands::cloud_agent::opencode::Connection {
                url: "https://agent.example.com".into(),
                username: "opencode".into(),
                password: "secret ' $(echo injected)".into(),
                directory: "/app/a project".into(),
                reused: true,
            };
            let mut pane = Session::spawn_client(
                "vm".into(),
                "box".into(),
                &binary,
                &client_sessions::Connection::OpenCode(c, beta),
                None,
                Some("ses_thread1"),
                None,
                24,
                80,
                || {},
            )
            .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !pane
                .with_screen(|s| s.contents().contains("ready"))
                .unwrap_or(false)
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{:?}",
                    pane.last_line()
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            pane.resize(30, 100);
            pane.write_raw(b"hello\n");
            while !pane.finished() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{:?}",
                    pane.last_line()
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(
                pane.durable_name,
                client_sessions::name(
                    if beta { "opencode2" } else { "opencode" },
                    "vm",
                    Some("ses_thread1")
                )
            );
        }
    }

    /// Drain a filter the way the reader thread does: push each chunk, then
    /// flush at EOF, concatenating everything the emulator would have seen.
    fn drain_filter(chunks: &[&[u8]]) -> Vec<u8> {
        let mut filter = BannerFilter::new();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend_from_slice(&filter.push(chunk));
        }
        out.extend_from_slice(&filter.flush());
        out
    }

    #[test]
    fn the_relay_announcement_never_reaches_the_emulator() {
        let out = drain_filter(&[b"Railway durable session: claude-abc123\r\nhello"]);
        assert_eq!(out, b"hello");
    }

    #[test]
    fn a_leading_blank_line_around_the_announcement_goes_with_it() {
        let out = drain_filter(&[b"\r\nRailway durable session: claude-abc123\r\nhello"]);
        assert_eq!(out, b"hello");
    }

    #[test]
    fn a_banner_split_across_reads_is_still_removed() {
        let out = drain_filter(&[b"Railway durable sess", b"ion: claude-abc123\r\nhello"]);
        assert_eq!(out, b"hello");
    }

    #[test]
    fn banner_bytes_are_held_back_not_fed_as_a_fragment() {
        let mut filter = BannerFilter::new();
        // A partial banner with no terminator feeds nothing yet — feeding the
        // fragment would leave "Railway dur" on the emulator's first row.
        assert!(filter.push(b"Railway dur").is_empty());
        let mut out = filter.push(b"able session: x-1\r\nok");
        out.extend_from_slice(&filter.flush());
        assert_eq!(out, b"ok");
        assert_eq!(filter.name.as_deref(), Some("x-1"));
    }

    #[test]
    fn relay_petname_updates_transport_without_replacing_the_thread() {
        let mut filter = BannerFilter::new();
        filter.push(b"Railway durable session exact-petname. Use CTRL + \\ then D to detach\r\n");
        let mut pane = Session::for_test("agent", "box").unwrap();
        pane.durable_name = client_sessions::draft_name("claude", "agent", "pane");
        *pane.announced_console.lock().unwrap() = filter.name.take();
        pane.sync_console_name();
        assert_eq!(pane.console_name.as_deref(), Some("exact-petname"));
        assert_eq!(
            pane.durable_name,
            client_sessions::draft_name("claude", "agent", "pane")
        );
        filter.push(b"Railway durable session forged\r\n");
        assert!(filter.name.is_none());
    }

    #[test]
    fn ordinary_output_flows_through_untouched() {
        let out = drain_filter(&[b"\x1b[?1049h\x1b[Hhello\r\nworld"]);
        assert_eq!(out, b"\x1b[?1049h\x1b[Hhello\r\nworld");
    }

    #[test]
    fn terminal_startup_queries_are_answered_without_waiting_for_more_output() {
        const BURST: &[u8] = b"\x1b[?2004h\x1b[?1004h\x1b[?u\x1b[c\x1b[6n";
        for banner in [
            b"".as_slice(),
            b"Railway durable session: railway-test\r\n",
            b"Railway durable session railway-test. Use CTRL + \\ then D to detach\r\n",
            b"An unrecognized relay greeting\r\n",
        ] {
            for chunk_size in 1..=BURST.len() {
                let mut filter = BannerFilter::new();
                let mut queries = TerminalReplies::default();
                let mut parser = pane_parser(24, 80, 0);
                let kitty = AtomicBool::new(false);
                let mut replies = Vec::new();
                for chunk in banner.chunks(chunk_size).chain(BURST.chunks(chunk_size)) {
                    let bytes = filter.push(chunk);
                    replies.extend(queries.process(&bytes, &mut parser, &kitty));
                }
                // Do not flush EOF: the harness is waiting for these replies
                // before it can draw its first frame or send another byte.
                assert!(replies.starts_with(b"\x1b[?0u\x1b[?62;22c"));
                assert!(replies.ends_with(b";1R"), "{replies:?}");
            }
        }
    }

    #[test]
    fn cursor_queries_are_answered_in_order_at_their_position() {
        let mut parser = pane_parser(24, 80, 0);
        let replies = TerminalReplies::default().process(
            b"hello\x1b[6n\r\nworld\x1b[6n",
            &mut parser,
            &AtomicBool::new(false),
        );
        assert_eq!(replies, b"\x1b[1;6R\x1b[2;6R");
    }

    #[test]
    fn codex_palette_probe_preserves_host_colors_and_reply_order_across_reads() {
        use terminal_colorsaurus::Color;

        for (fg, bg) in [
            (
                Color::rgb(0xeeee, 0xdddd, 0xcccc),
                Color::rgb(0x1234, 0x2345, 0x3456),
            ),
            (
                Color::rgb(0x1111, 0x2222, 0x3333),
                Color::rgb(0xffff, 0xfafa, 0xefef),
            ),
        ] {
            for terminator in ["\x07", "\x1b\\"] {
                // Codex probes the palette before DA1, which closes its probe.
                // Also interleave cursor and Kitty queries to catch reordering.
                let burst = format!(
                    "hi\x1b[6n\x1b]10;?{terminator}\x1b]11;?{terminator}\x1b[0c\x1b[?u\r\nbye\x1b[6n"
                );
                let expected = format!(
                    "\x1b[1;3R\x1b]10;rgb:{:04x}/{:04x}/{:04x}\x1b\\\x1b]11;rgb:{:04x}/{:04x}/{:04x}\x1b\\\x1b[?62;22c\x1b[?0u\x1b[2;4R",
                    fg.r, fg.g, fg.b, bg.r, bg.g, bg.b
                );
                for chunk_size in 1..=burst.len() {
                    let mut parser = pane_parser(24, 80, 0);
                    parser.callbacks_mut().colors = Some(terminal_palette::DefaultColors {
                        fg: fg.clone(),
                        bg: bg.clone(),
                    });
                    let mut queries = TerminalReplies::default();
                    let mut filter = BannerFilter::new();
                    let kitty = AtomicBool::new(false);
                    let mut replies = Vec::new();
                    for chunk in burst.as_bytes().chunks(chunk_size) {
                        replies.extend(queries.process(&filter.push(chunk), &mut parser, &kitty));
                    }
                    assert_eq!(replies, expected.as_bytes(), "chunk size {chunk_size}");
                    assert_eq!(parser.screen().contents(), "hi\nbye");
                    assert!(parser.callbacks().replies.is_empty());
                }
            }
        }
    }

    #[test]
    fn palette_queries_need_no_following_csi_and_color_setters_stay_local() {
        use terminal_colorsaurus::Color;

        let mut parser = pane_parser(24, 80, 0);
        parser.callbacks_mut().colors = Some(terminal_palette::DefaultColors {
            fg: Color::rgb(0xffff, 0xffff, 0xffff),
            bg: Color::rgb(0x1234, 0x2345, 0x3456),
        });
        let mut queries = TerminalReplies::default();
        let kitty = AtomicBool::new(false);
        assert!(queries.process(
            b"\x1b]11;rgb:ffff/ffff/ffff\x07\x1b]0;title\x07\x1b]8;;https://example.com\x1b\\\x1b]52;c;?\x07",
            &mut parser,
            &kitty,
        ).is_empty());
        assert_eq!(
            queries.process(b"\x1b]11;?\x1b\\", &mut parser, &kitty),
            b"\x1b]11;rgb:1234/2345/3456\x1b\\"
        );
        assert!(parser.screen().contents().is_empty());
    }

    #[test]
    fn unavailable_host_palette_does_not_invent_colors_or_block_other_queries() {
        let mut parser = pane_parser(24, 80, 0);
        parser.callbacks_mut().colors = None;
        let replies = TerminalReplies::default().process(
            b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b[c\x1b[?u\x1b[6n",
            &mut parser,
            &AtomicBool::new(false),
        );
        assert_eq!(replies, b"\x1b[?62;22c\x1b[?0u\x1b[1;1R");
        assert!(parser.screen().contents().is_empty());
    }

    #[test]
    fn identical_text_after_the_announcement_is_kept() {
        // Only the relay's own announcement is stripped: the same words typed
        // or echoed later are session content.
        let out = drain_filter(&[
            b"Railway durable session: one\r\n",
            b"echo Railway durable session: two\r\n",
        ]);
        assert_eq!(out, b"echo Railway durable session: two\r\n");
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
        // typed text, which is worse than the ambiguity it replaces. ⇧enter
        // still must not submit, so it falls back to the legacy newline chord.
        assert_eq!(bytes(enter(KeyModifiers::SHIFT), false), b"\x1b\r");
        assert_eq!(bytes(enter(KeyModifiers::ALT), false), b"\x1b\r");
        // Ctrl is not a newline in anyone's legacy vocabulary — leave it alone.
        assert_eq!(bytes(enter(KeyModifiers::CONTROL), false), b"\r");

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

    /// Reconnecting resolves the relay plumbing from the environment, which
    /// the session only holds inside its relay target.
    #[test]
    fn the_environment_is_read_back_out_of_the_relay_target() {
        let session = Session::for_test("ca", "test").unwrap();
        // for_test connects as agent:test:test.
        assert_eq!(session.environment_id().as_deref(), Some("test"));
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

    /// A paste reaches the pty the way a real terminal would send it: marker-
    /// wrapped when the program switched bracketed paste on, bare when it
    /// never did, and newlines as CR either way — dictated text with a line
    /// break must not hit Enter mid-thought.
    #[test]
    fn paste_encodes_for_the_mode_the_program_asked_for() {
        assert_eq!(encode_paste("ship it", true), b"\x1b[200~ship it\x1b[201~");
        assert_eq!(encode_paste("ship it", false), b"ship it");
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(encode_paste("a\nb", true), b"\x1b[200~a\rb\x1b[201~");
    }

    /// The end marker cannot ride a paste out of its brackets and turn the
    /// rest of the clipboard into live keystrokes.
    #[test]
    fn paste_cannot_smuggle_its_own_end_marker() {
        assert_eq!(
            encode_paste("safe\x1b[201~rm -rf /\r", true),
            b"\x1b[200~saferm -rf /\r\x1b[201~"
        );
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
    fn top_scrolling_regions_preserve_history_and_the_fixed_composer() {
        let mut parser = pane_parser(6, 30, 20);
        parser.process(b"\x1b[5;1Hcomposer\x1b[6;1Hfooter\x1b[1;4r\x1b[1;1H");
        for i in 0..40 {
            parser.process(format!("\x1b[31mline-{i:02}\x1b[0m\r\n").as_bytes());
        }
        assert_eq!(parser.screen().cell(4, 0).unwrap().contents(), "c");
        assert_eq!(parser.screen().cell(5, 0).unwrap().contents(), "f");
        assert!(parser.screen().contents().contains("line-39"));
        parser.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(
            parser.screen().scrollback(),
            20,
            "retention remains bounded"
        );
        assert!(parser.screen().contents().contains("line-17"));
        assert_eq!(
            parser.screen().cell(0, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
        let history = parser.screen().contents();
        // New output keeps the scrolled view anchored and the composer intact.
        parser.process(b"line-40\r\n");
        assert!(parser.screen().scrollback() > 0);
        assert_ne!(
            parser.screen().contents(),
            history,
            "oldest retained row was evicted"
        );
        parser.screen_mut().set_scrollback(0);
        assert!(parser.screen().contents().contains("line-40"));
        assert_eq!(parser.screen().cell(4, 0).unwrap().contents(), "c");
        assert_eq!(parser.screen().cell(5, 0).unwrap().contents(), "f");
    }

    #[test]
    fn scrolling_below_a_header_and_on_alternate_screens_stays_out_of_history() {
        for setup in [
            b"\x1b[2;4r\x1b[2;1H".as_slice(),
            b"\x1b[?1049h\x1b[1;4r\x1b[1;1H".as_slice(),
        ] {
            let mut parser = pane_parser(6, 30, 20);
            parser.process(setup);
            for i in 0..40 {
                parser.process(format!("line-{i:02}\r\n").as_bytes());
            }
            parser.screen_mut().set_scrollback(usize::MAX);
            assert_eq!(parser.screen().scrollback(), 0);
        }
    }

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
            // The reader thread may process this round's echo between the
            // scroll above and this read, and output arriving while scrolled
            // back pins the view by pushing the emulator's offset deeper. So
            // the emulator may run ahead of the held offset here — but it can
            // never sit above it, which is the wedge this test is for.
            assert!(
                history >= held,
                "the held offset never passes the emulator (held {held}, emulator {history})"
            );
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

    /// The drain path under flood, the shape of a reattach replaying a long
    /// session's recording: megabytes of output while the emulator lock is
    /// hammered from the render side, the way a frame loop does. The reader
    /// thread is the only thing draining the pty — if it falls behind, the
    /// far side backs up until ssh's keepalive replies stop arriving and ssh
    /// kills the connection. So: everything must land, promptly, with the
    /// session alive and history still bounded by the emulator's retention.
    /// Unix only: the fixture's pty is ConPTY on Windows, which cooks and
    /// throttles the byte stream instead of pumping it raw — the megabytes
    /// never make it through inside any reasonable deadline, and the drain
    /// path under test is the raw one a real ssh session rides.
    #[cfg(unix)]
    #[test]
    fn a_flood_drains_under_render_contention() {
        let mut session = Session::for_test("ca", "test").unwrap();
        session.resize(40, 200);

        // ~4MB through the pty (each byte travels twice: written in, echoed
        // back out), in line-sized writes because the fixture pty is
        // line-buffered.
        let line = "x".repeat(196);
        let started = std::time::Instant::now();
        for i in 0..20_000 {
            session.send(format!("{line}\r\n").as_bytes());
            // The render side of the contention: a frame loop reading the
            // screen between chunks, holding the same lock the reader needs.
            if i % 50 == 0 {
                let _ = session.with_screen(|s| s.contents());
            }
        }
        session.send(b"FLOOD-DRAINED-MARKER\r\n");

        // The whole flood, plus its echo, has to come out the other side.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut seen = false;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if session
                .with_screen(|s| s.contents().contains("FLOOD-DRAINED-MARKER"))
                .unwrap_or(false)
            {
                seen = true;
                break;
            }
        }
        assert!(seen, "the flood never finished draining");
        assert!(!session.ended(), "a flood must not kill the session");
        // Memory stays bounded: retention clamps, however much flowed through.
        let history = session.with_screen(|s| s.scrollback());
        session.scroll_by(isize::MAX);
        assert!(
            session.scroll <= 4000,
            "history is clamped at retention, not the flood's size (scroll {}, scrollback {history:?})",
            session.scroll
        );
        eprintln!(
            "flood drained in {:?} ({} lines)",
            started.elapsed(),
            20_000
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

    #[test]
    fn da1_is_answered_in_either_spelling() {
        let reply = Some(b"\x1b[?62;22c".to_vec());
        assert_eq!(da1_reply(b"\x1b[c"), reply);
        assert_eq!(da1_reply(b"\x1b[0c"), reply);
        // Anywhere in the chunk, including after sequences that are not it.
        assert_eq!(da1_reply(b"\x1b[?2004h\x1b[?u\x1b[c\x1b[6n"), reply);
    }

    /// The `c` final byte is common and the introducer is everywhere, so this
    /// is the scanner most able to answer a question nobody asked.
    #[test]
    fn da1_reply_is_none_without_a_query() {
        assert_eq!(da1_reply(b"just some output\r\n"), None);
        // A terminal's own DA1 response is not a request for one.
        assert_eq!(da1_reply(b"\x1b[?62;22c"), None);
        // Neither is any other sequence that happens to end in `c`, nor a `c`
        // in plain text after an unrelated escape sequence.
        assert_eq!(da1_reply(b"\x1b[38;5;2mcyan code\x1b[0m"), None);
        assert_eq!(da1_reply(b"\x1b[2J\x1b[Hcat"), None);
    }

    /// The startup burst `railway-agent-tui` actually sends, in a single write:
    /// two mode sets, the kitty query, DA1, then the cursor query. Answering
    /// the kitty query and not DA1 is what left the pane empty — crossterm
    /// waits on DA1 as its sentinel — so all three answers have to come back,
    /// in the order they were asked.
    #[test]
    fn the_harness_startup_burst_gets_every_answer() {
        const BURST: &[u8] = b"\x1b[?2004h\x1b[?1004h\x1b[?u\x1b[c\x1b[6n";
        let kitty = AtomicBool::new(false);
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(BURST);

        let mut replies = kitty_scan(BURST, &kitty).expect("the kitty query is answered");
        replies.extend_from_slice(&da1_reply(BURST).expect("DA1 is answered"));
        replies.extend_from_slice(
            &dsr_reply(BURST, parser.screen()).expect("the cursor query is answered"),
        );
        assert_eq!(replies, b"\x1b[?0u\x1b[?62;22c\x1b[1;1R");
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
