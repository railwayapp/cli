//! The host terminal's default colors, shared by the embedded terminals.
//!
//! Codex uses OSC 10/11 to derive its prompt/plan backgrounds and accents. The
//! pane leaves default colors transparent, so these must be the host's colors,
//! rather than the Railway chrome's theme or an assumed dark palette.

use std::io::IsTerminal;
use std::sync::OnceLock;

use terminal_colorsaurus::{Color, QueryOptions};

#[derive(Clone)]
pub(super) struct DefaultColors {
    pub fg: Color,
    pub bg: Color,
}

static COLORS: OnceLock<Option<DefaultColors>> = OnceLock::new();

/// Call before crossterm starts reading input. Query once, including across TUI
/// re-entry; PTY reader threads only use the cached result. Colorsaurus bounds
/// the probe and restores terminal modes on both success and failure.
pub(super) fn capture() {
    COLORS.get_or_init(|| {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return None;
        }
        let palette = terminal_colorsaurus::color_palette(QueryOptions::default()).ok()?;
        Some(DefaultColors {
            fg: palette.foreground,
            bg: palette.background,
        })
    });
}

pub(super) fn cached() -> Option<DefaultColors> {
    COLORS.get().cloned().flatten()
}
