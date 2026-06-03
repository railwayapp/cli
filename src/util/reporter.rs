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

/// Emit a non-fatal warning. Always goes to stderr so it never pollutes
/// a result on stdout: a yellow line in human mode, a structured object
/// in JSON mode.
pub fn warn(code: &str, message: impl std::fmt::Display, hint: Option<&str>) {
    match mode() {
        OutputMode::Json => {
            let obj = serde_json::json!({
                "level": "warning",
                "code": code,
                "message": message.to_string(),
                "hint": hint,
            });
            eprintln!("{obj}");
        }
        OutputMode::Human => {
            eprintln!("{} {message}", "warning:".yellow().bold());
            if let Some(hint) = hint {
                eprintln!("  {} {hint}", "→".cyan());
            }
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
