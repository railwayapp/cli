//! Output reporting: a single place that knows the process output mode
//! and how to emit results, warnings, and errors consistently.
//!
//! Stream contract:
//! - **stdout** carries result data only — a single JSON object (or an
//!   NDJSON stream for streaming commands) on success, or a single JSON
//!   error object on failure. The two are mutually exclusive.
//! - **stderr** carries human progress, structured warnings, and (in
//!   human mode) the human-readable error.
//! - the exit code signals success/failure.
//!
//! This lets an agent always parse stdout regardless of outcome, while
//! humans get readable progress on stderr.

use std::sync::OnceLock;

use colored::Colorize;
use serde::Serialize;

use crate::errors::RailwayError;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputMode {
    Human,
    Json,
}

static MODE: OnceLock<OutputMode> = OnceLock::new();

/// Set the process-wide output mode once, at command entry, from the
/// command's `--json` flag. Only commands that support JSON need to call
/// this; everything else defaults to `Human`. The runtime is async, so
/// this is a process global (a thread-local would not survive `.await`
/// across tokio worker threads). Set once per process — a second call is
/// a no-op.
pub fn set_mode(json: bool) {
    let _ = MODE.set(if json {
        OutputMode::Json
    } else {
        OutputMode::Human
    });
}

pub fn mode() -> OutputMode {
    MODE.get().copied().unwrap_or(OutputMode::Human)
}

/// Emit a result value on stdout. In JSON mode this is the single
/// machine-readable result line. Human rendering stays in the command —
/// this is the sanctioned primitive for the JSON side of the contract.
///
/// Introduced ahead of broad adoption: existing commands still emit
/// JSON ad-hoc, and they should migrate onto this over time rather than
/// hand-rolling `println!("{}", serde_json::json!(…))`.
#[allow(dead_code)]
pub fn emit_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

/// Warnings raised while a TUI owned the terminal, waiting for it to give the
/// terminal back. Deduplicated by rendered text and kept in first-seen order,
/// so a condition that recurs on a timer surfaces once with a count instead of
/// as a wall of identical lines.
static DEFERRED: std::sync::Mutex<Vec<(String, usize)>> = std::sync::Mutex::new(Vec::new());

/// Cap on distinct deferred warnings. A TUI session runs for hours; without a
/// bound, a warning raised from a loop would hold unbounded memory for all of
/// it. Past the cap, further *distinct* warnings are dropped — repeats of ones
/// already held still count up, which is the case that actually matters.
const MAX_DEFERRED: usize = 20;

/// Emit a non-fatal warning. Always goes to stderr so it never pollutes
/// a result on stdout: a yellow line in human mode, a structured object
/// in JSON mode.
///
/// Held back while a full-screen TUI owns the terminal, and released by
/// [`flush_deferred`] when it exits. Nothing reads stderr while a TUI is up —
/// ratatui owns the alternate screen and paints from its own buffer — so a
/// warning written then does not inform anyone, it corrupts the frame: the
/// text lands wherever the cursor happens to be and survives until something
/// forces a full repaint. A long-lived `railway ca` session could accumulate
/// hundreds of them across the whole screen. Deferring rather than discarding
/// keeps the diagnostic; it just arrives when there is a terminal to read it
/// on.
pub fn warn(code: &str, message: impl std::fmt::Display, hint: Option<&str>) {
    let rendered = render_warning(code, message, hint, mode());
    if crate::util::prompt::terminal_owned() {
        defer(rendered);
        return;
    }
    eprint!("{rendered}");
}

/// The exact bytes a warning writes to stderr, including the trailing newline.
/// Rendering up front is what lets the deferred path dedupe on the final text
/// and replay it verbatim later.
fn render_warning(
    code: &str,
    message: impl std::fmt::Display,
    hint: Option<&str>,
    mode: OutputMode,
) -> String {
    match mode {
        OutputMode::Json => {
            let obj = serde_json::json!({
                "level": "warning",
                "code": code,
                "message": message.to_string(),
                "hint": hint,
            });
            format!("{obj}\n")
        }
        OutputMode::Human => {
            let mut out = format!("{} {message}\n", "warning:".yellow().bold());
            if let Some(hint) = hint {
                out.push_str(&format!("  {} {hint}\n", "→".cyan()));
            }
            out
        }
    }
}

fn defer(rendered: String) {
    let Ok(mut held) = DEFERRED.lock() else {
        return;
    };
    if let Some(entry) = held.iter_mut().find(|(text, _)| *text == rendered) {
        entry.1 += 1;
    } else if held.len() < MAX_DEFERRED {
        held.push((rendered, 1));
    }
}

/// Write out everything [`warn`] held back while a TUI owned the terminal, and
/// forget it. Called from `prompt::set_terminal_owned(false)`, so every TUI's
/// `restore_terminal` releases them without needing to know they exist.
pub fn flush_deferred() {
    let Ok(mut held) = DEFERRED.lock() else {
        return;
    };
    for (rendered, count) in held.drain(..) {
        eprint!("{rendered}");
        if count > 1 {
            eprintln!(
                "  {}",
                format!("(repeated {count} times while the screen was open)").dimmed()
            );
        }
    }
}

enum Stream {
    Stdout,
    Stderr,
}

/// Pure rendering of a fatal error for a given mode: returns the target
/// stream and the exact text to write. Kept separate from the IO so it
/// can be unit-tested without touching the process-global mode or
/// capturing real stdio.
fn render_error_message(err: &anyhow::Error, mode: OutputMode) -> (Stream, String) {
    match mode {
        OutputMode::Json => {
            let (code, hint) = match err.downcast_ref::<RailwayError>() {
                Some(railway_err) => (railway_err.code(), railway_err.hint()),
                None => ("ERROR", None),
            };
            let obj = serde_json::json!({
                "error": err.to_string(),
                "code": code,
                "hint": hint,
            });
            (Stream::Stdout, obj.to_string())
        }
        OutputMode::Human => {
            // Keep the existing debug-formatted message (incl. anyhow's
            // context chain), then surface the RailwayError hint so the
            // actionable next step isn't lost in human mode.
            let mut text = format!("{err:?}");
            if let Some(hint) = err
                .downcast_ref::<RailwayError>()
                .and_then(RailwayError::hint)
            {
                text.push_str(&format!("\n  {} {hint}", "→".cyan()));
            }
            (Stream::Stderr, text)
        }
    }
}

/// Render a fatal error at the top level (called from `main`). In JSON
/// mode the error object goes to stdout in place of a result (stream
/// contract); in human mode it goes to stderr — keeping the debug-
/// formatted message and appending the actionable hint when present.
pub fn render_error(err: &anyhow::Error) {
    match render_error_message(err, mode()) {
        (Stream::Stdout, text) => println!("{text}"),
        (Stream::Stderr, text) => eprintln!("{text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::RailwayError;

    /// `DEFERRED` is process-global, so the tests that drive it directly have
    /// to run one at a time or they consume each other's entries.
    static DEFERRED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A warning raised under a TUI must survive to be read, not vanish — and a
    /// condition that recurs on a timer (the config-lock timeout fired every
    /// few seconds in a long `railway ca` session) must come back as one line
    /// with a count, which is the whole reason it is deduplicated rather than
    /// queued.
    #[test]
    fn deferred_warnings_are_deduplicated_and_counted() {
        let _guard = DEFERRED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flush_deferred(); // start from a clean slate

        defer("lock timed out\n".to_string());
        defer("lock timed out\n".to_string());
        defer("lock timed out\n".to_string());
        defer("something else\n".to_string());

        let held = DEFERRED.lock().unwrap().clone();
        assert_eq!(
            held,
            vec![
                ("lock timed out\n".to_string(), 3),
                ("something else\n".to_string(), 1),
            ],
            "repeats collapse into a count, and first-seen order is kept"
        );

        flush_deferred();
        assert!(
            DEFERRED.lock().unwrap().is_empty(),
            "flushing must drain, or the next TUI exit replays them"
        );
    }

    /// An unbounded buffer would be a slow leak across an hours-long session.
    #[test]
    fn deferred_warnings_are_capped() {
        let _guard = DEFERRED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flush_deferred();

        for i in 0..(MAX_DEFERRED + 10) {
            defer(format!("distinct warning {i}\n"));
        }
        // The cap bounds distinct entries, but a repeat of one already held
        // still counts up — that is the case worth keeping.
        defer("distinct warning 0\n".to_string());

        let held = DEFERRED.lock().unwrap().clone();
        assert_eq!(held.len(), MAX_DEFERRED);
        assert_eq!(held[0].1, 2);

        flush_deferred();
    }

    #[test]
    fn human_error_surfaces_railway_hint() {
        let err: anyhow::Error = RailwayError::NotAuthenticated.into();
        let (stream, text) = render_error_message(&err, OutputMode::Human);
        assert!(matches!(stream, Stream::Stderr));
        assert!(text.contains("Not signed in."));
        // Regression guard: the actionable hint must not be lost in
        // human mode just because it lives in hint() not the message.
        assert!(text.contains("railway login"));
    }

    #[test]
    fn human_error_without_hint_is_just_the_message() {
        let err: anyhow::Error = RailwayError::NoProjects.into();
        let (stream, text) = render_error_message(&err, OutputMode::Human);
        assert!(matches!(stream, Stream::Stderr));
        // NoProjects has no hint(), so there's no trailing arrow line.
        assert!(!text.contains('→'));
    }

    #[test]
    fn json_error_includes_code_and_hint_on_stdout() {
        let err: anyhow::Error = RailwayError::NotAuthenticated.into();
        let (stream, text) = render_error_message(&err, OutputMode::Json);
        assert!(matches!(stream, Stream::Stdout));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["code"], "NOT_AUTHENTICATED");
        assert_eq!(v["error"], "Not signed in.");
        assert!(v["hint"].as_str().unwrap().contains("railway login"));
    }

    #[test]
    fn json_error_for_generic_anyhow_uses_error_bucket() {
        let err = anyhow::anyhow!("boom");
        let (_stream, text) = render_error_message(&err, OutputMode::Json);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["code"], "ERROR");
        assert_eq!(v["error"], "boom");
        assert!(v["hint"].is_null());
    }
}
