//! Shared colour themes for every ratatui screen in the CLI.
//!
//! Originally built for the `railway ca` TUI (see the module's history in
//! `commands/cloud_agent/tui/theme.rs`), then generalized here so `metrics`,
//! `railway scale`, the volume file browser, `dev`/`run`, and `templates` can
//! all draw from the same values instead of each hardcoding its own
//! `Color::Cyan` / `Color::Yellow` / `Color::DarkGray` constants. Truecolor
//! for the designed themes — the relay already assumes a modern terminal, and
//! Railway's violet has no ANSI-16 equivalent worth approximating — plus one
//! theme built entirely from the sixteen named colours, which is what to use
//! when the terminal's own palette should win.
//!
//! `cloud_agent::tui::theme` re-exports this module so its existing consumers
//! (`use super::theme::{Theme, THEMES}`) keep working unchanged.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ratatui::style::Color;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    /// Stable identifier, persisted in the shared TUI preferences file.
    pub slug: &'static str,
    pub label: &'static str,
    /// Wordmark, selected rows, focused borders, key badges. Also the
    /// general-purpose "active but not urgent" accent — a border or label
    /// that would otherwise reach for a raw `Color::Cyan`.
    pub accent: Color,
    /// Unfocused borders and hint lines.
    pub accent_dim: Color,
    /// Secondary text.
    pub dim: Color,
    /// Primary text.
    pub fg: Color,
    /// Text drawn on top of `accent` — must stay legible there.
    pub on_accent: Color,
    /// Fill for dialogs, cards and input boxes. Opaque on purpose: an overlay
    /// that lets the screen underneath show through reads as a rendering bug
    /// rather than as something sitting on top of the screen. A step lighter
    /// than the page it floats over, so it reads as raised.
    pub surface: Color,
    /// Solid neutral background for the resizable navigation sidebar.
    pub sidebar: Color,
    /// Row highlight in the tree.
    pub selection: Color,
    /// Also doubles as `success` outside cloud-agent (2xx-ish good states,
    /// an Apply button): nothing about "a session is running" and "this
    /// state is good" needs different colours.
    pub running: Color,
    pub sleeping: Color,
    /// Also doubles as `warning` outside cloud-agent (4xx, p90, "resize your
    /// terminal" banners).
    pub pending: Color,
    /// Delete confirmations, 5xx / p99, a Cancel button — the one role none
    /// of cloud-agent's original fields covered.
    pub danger: Color,
    /// An ordered palette for charts that draw several simultaneous series
    /// (CPU/memory limits aside, which reuse `accent`) — egress/ingress,
    /// p50/p90/p95/p99, 2xx/3xx/4xx/5xx. Picked by index rather than named
    /// per metric, so a chart with N series just takes `series[0..N]`
    /// instead of the metrics TUI naming a colour per metric itself.
    pub series: &'static [Color],
}

pub const THEMES: &[Theme] = &[
    // Railway: the plum background of the dashboard with its violet accent.
    Theme {
        slug: "railway",
        label: "Railway",
        accent: Color::Rgb(0xc4, 0xa8, 0xff),
        accent_dim: Color::Rgb(0x6b, 0x5b, 0x8f),
        dim: Color::Rgb(0x8b, 0x84, 0x9c),
        fg: Color::Rgb(0xef, 0xec, 0xf6),
        on_accent: Color::Rgb(0x18, 0x12, 0x28),
        surface: Color::Rgb(0x25, 0x1d, 0x39),
        sidebar: Color::Rgb(0x1c, 0x1d, 0x21),
        selection: Color::Rgb(0x37, 0x2a, 0x50),
        running: Color::Rgb(0x6e, 0xe7, 0xa8),
        sleeping: Color::Rgb(0x7d, 0x77, 0x8f),
        pending: Color::Rgb(0xf5, 0xc0, 0x6b),
        danger: Color::Rgb(0xf2, 0x6d, 0x6d),
        series: &[
            Color::Rgb(0x6d, 0x9d, 0xf2),
            Color::Rgb(0xf5, 0xc0, 0x6b),
            Color::Rgb(0xc9, 0x6b, 0xf2),
            Color::Rgb(0xf2, 0x6d, 0x6d),
        ],
    },
    // Terminal: no opinion about colour at all — everything resolves through
    // the user's own sixteen. The right choice on a themed or low-colour
    // terminal, and the safe fallback if truecolor ever misbehaves.
    Theme {
        slug: "terminal",
        label: "Terminal (ANSI)",
        accent: Color::Cyan,
        accent_dim: Color::DarkGray,
        dim: Color::DarkGray,
        fg: Color::White,
        on_accent: Color::Black,
        // The theme that defers to the terminal's palette, so a dialog fills
        // with the terminal's own background rather than a colour of ours.
        surface: Color::Black,
        // Nothing darker than black to fall back on, so the shade is a grey
        // one — the only depth cue the sixteen colours can carry.
        sidebar: Color::Black,
        selection: Color::DarkGray,
        running: Color::Green,
        sleeping: Color::Gray,
        pending: Color::Yellow,
        danger: Color::Red,
        series: &[Color::Blue, Color::Yellow, Color::Magenta, Color::Red],
    },
    // Ember: warm amber on near-black.
    Theme {
        slug: "ember",
        label: "Ember",
        accent: Color::Rgb(0xf5, 0xa5, 0x24),
        accent_dim: Color::Rgb(0x7a, 0x54, 0x1c),
        dim: Color::Rgb(0x93, 0x86, 0x76),
        fg: Color::Rgb(0xf6, 0xf1, 0xe8),
        on_accent: Color::Rgb(0x24, 0x18, 0x06),
        surface: Color::Rgb(0x2c, 0x22, 0x14),
        sidebar: Color::Rgb(0x1c, 0x1d, 0x21),
        selection: Color::Rgb(0x45, 0x2d, 0x10),
        running: Color::Rgb(0x9a, 0xd8, 0x6a),
        sleeping: Color::Rgb(0x86, 0x7c, 0x6e),
        pending: Color::Rgb(0xf5, 0xd2, 0x6b),
        danger: Color::Rgb(0xe0, 0x5a, 0x4a),
        series: &[
            Color::Rgb(0x6a, 0x8c, 0xd6),
            Color::Rgb(0xf5, 0xd2, 0x6b),
            Color::Rgb(0xd6, 0x8c, 0xd6),
            Color::Rgb(0xe0, 0x5a, 0x4a),
        ],
    },
    // Mono: greyscale, for screenshots, recordings, and anyone who wants the
    // structure to carry the meaning instead of the colour.
    Theme {
        slug: "mono",
        label: "Mono",
        accent: Color::Rgb(0xe6, 0xe6, 0xe6),
        accent_dim: Color::Rgb(0x6a, 0x6a, 0x6a),
        dim: Color::Rgb(0x8a, 0x8a, 0x8a),
        fg: Color::Rgb(0xf2, 0xf2, 0xf2),
        on_accent: Color::Rgb(0x10, 0x10, 0x10),
        surface: Color::Rgb(0x23, 0x23, 0x23),
        sidebar: Color::Rgb(0x1c, 0x1d, 0x21),
        selection: Color::Rgb(0x33, 0x33, 0x33),
        running: Color::Rgb(0xe6, 0xe6, 0xe6),
        sleeping: Color::Rgb(0x77, 0x77, 0x77),
        pending: Color::Rgb(0xb4, 0xb4, 0xb4),
        danger: Color::Rgb(0xcf, 0xcf, 0xcf),
        series: &[
            Color::Rgb(0xd8, 0xd8, 0xd8),
            Color::Rgb(0xb4, 0xb4, 0xb4),
            Color::Rgb(0x90, 0x90, 0x90),
            Color::Rgb(0xe6, 0xe6, 0xe6),
        ],
    },
];

impl Theme {
    pub fn default_theme() -> &'static Theme {
        &THEMES[0]
    }

    /// Look up by slug, falling back to the default — the preferences file is
    /// hand-editable and a typo there should change nothing.
    pub fn from_slug(slug: Option<&str>) -> &'static Theme {
        slug.and_then(|s| THEMES.iter().find(|t| t.slug == s))
            .unwrap_or_else(Self::default_theme)
    }

    pub fn index(&self) -> usize {
        THEMES.iter().position(|t| t.slug == self.slug).unwrap_or(0)
    }

    pub fn next(&self) -> &'static Theme {
        &THEMES[(self.index() + 1) % THEMES.len()]
    }

    /// The theme every screen should open with: the shared preference at
    /// `~/.railway/tui-prefs.json`, migrated once from cloud-agent's older
    /// `agent-prefs.json` if that's the only place a choice exists yet, or
    /// the default if neither file says otherwise.
    pub fn load_preference() -> &'static Theme {
        match dirs::home_dir() {
            Some(home) => Self::load_preference_in(&home),
            None => Self::default_theme(),
        }
    }

    fn load_preference_in(home: &Path) -> &'static Theme {
        if let Ok(raw) = std::fs::read_to_string(TuiPrefs::path_in(home)) {
            let slug = serde_json::from_str::<TuiPrefs>(&raw)
                .ok()
                .and_then(|prefs| prefs.theme);
            return Self::from_slug(slug.as_deref());
        }

        match migrate_theme_from_agent_prefs(home) {
            Some(slug) => {
                let theme = Self::from_slug(Some(&slug));
                // Best-effort: a failed seed just means the migration is
                // retried on the next read rather than the theme being lost.
                let _ = theme.save_preference_in(home);
                theme
            }
            None => Self::default_theme(),
        }
    }

    /// Persist this theme as the choice every screen opens with next.
    pub fn save_preference(&self) -> Result<()> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        self.save_preference_in(&home)
    }

    fn save_preference_in(&self, home: &Path) -> Result<()> {
        let path = TuiPrefs::path_in(home);
        let prefs = TuiPrefs {
            theme: Some(self.slug.to_string()),
        };
        let contents =
            serde_json::to_string_pretty(&prefs).context("Failed to serialize TUI preferences")?;
        crate::util::write_atomic(&path, &contents)
            .with_context(|| format!("Failed to write {}", path.display()))
    }
}

/// `~/.railway/tui-prefs.json` — the theme choice shared by every ratatui
/// screen. Deliberately its own file rather than a field cloud-agent's
/// `agent-prefs.json`: that file is cloud-agent's launch config, and a
/// property every screen reads shouldn't live inside one screen's own
/// preferences.
#[derive(Serialize, Deserialize, Default)]
struct TuiPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme: Option<String>,
}

impl TuiPrefs {
    fn path_in(home: &Path) -> PathBuf {
        home.join(".railway").join("tui-prefs.json")
    }
}

/// Read cloud-agent's older `theme` preference directly as JSON, rather than
/// depending on `commands::cloud_agent::prefs::AgentPrefs` — a top-level
/// module shared by `controllers/*` and `commands/cloud_agent/*` reaching
/// back into the latter would be the same backwards dependency this module
/// exists to avoid.
fn migrate_theme_from_agent_prefs(home: &Path) -> Option<String> {
    let path = home.join(".railway").join("agent-prefs.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("theme")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_unique_and_resolvable() {
        let mut seen = std::collections::HashSet::new();
        for theme in THEMES {
            assert!(seen.insert(theme.slug), "duplicate slug {}", theme.slug);
            assert_eq!(Theme::from_slug(Some(theme.slug)).slug, theme.slug);
        }
    }

    #[test]
    fn an_unknown_or_missing_slug_falls_back_to_railway() {
        assert_eq!(Theme::from_slug(None).slug, "railway");
        assert_eq!(Theme::from_slug(Some("nonsense")).slug, "railway");
        assert_eq!(Theme::default_theme().slug, "railway");
    }

    #[test]
    fn cycling_visits_every_theme_and_wraps() {
        let mut theme = Theme::default_theme();
        let mut seen = vec![theme.slug];
        for _ in 1..THEMES.len() {
            theme = theme.next();
            seen.push(theme.slug);
        }
        assert_eq!(seen.len(), THEMES.len());
        assert_eq!(theme.next().slug, "railway", "cycling wraps to the first");
    }

    /// Text drawn on the accent must not be the accent.
    #[test]
    fn on_accent_contrasts_with_accent() {
        for theme in THEMES {
            assert_ne!(theme.on_accent, theme.accent, "{}", theme.slug);
            assert_ne!(theme.fg, theme.selection, "{}", theme.slug);
        }
    }

    /// A dialog's fill has to differ from everything drawn on top of it, or the
    /// text disappears into the box.
    #[test]
    fn the_surface_contrasts_with_what_sits_on_it() {
        for theme in THEMES {
            assert_ne!(theme.surface, theme.fg, "{}", theme.slug);
            assert_ne!(theme.surface, theme.dim, "{}", theme.slug);
            assert_ne!(theme.surface, theme.accent, "{}", theme.slug);
            assert_ne!(theme.surface, theme.selection, "{}", theme.slug);
        }
    }

    /// `danger` has to read as its own thing next to the other semantic
    /// colours it sits alongside, or a delete confirmation and a healthy
    /// state become the same colour.
    #[test]
    fn danger_is_distinct_from_the_other_semantic_colours() {
        for theme in THEMES {
            assert_ne!(theme.danger, theme.running, "{}", theme.slug);
            assert_ne!(theme.danger, theme.pending, "{}", theme.slug);
            assert_ne!(theme.danger, theme.surface, "{}", theme.slug);
        }
    }

    /// A chart that draws N series takes `series[0..N]` — they have to be
    /// distinguishable from each other, and from the surface they're drawn
    /// on, or the lines merge into an unreadable smear.
    #[test]
    fn series_has_at_least_four_distinct_colours() {
        for theme in THEMES {
            assert!(
                theme.series.len() >= 4,
                "{} needs at least 4 series colours",
                theme.slug
            );
            for (i, a) in theme.series.iter().enumerate() {
                assert_ne!(*a, theme.surface, "{} series[{i}]", theme.slug);
                for b in &theme.series[i + 1..] {
                    assert_ne!(a, b, "{} has a repeated series colour", theme.slug);
                }
            }
        }
    }

    #[test]
    fn missing_preference_files_fall_back_to_default() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(Theme::load_preference_in(home.path()).slug, "railway");
    }

    #[test]
    fn preference_round_trips_through_disk() {
        let home = tempfile::tempdir().unwrap();
        let ember = Theme::from_slug(Some("ember"));
        ember.save_preference_in(home.path()).unwrap();
        assert_eq!(Theme::load_preference_in(home.path()).slug, "ember");
    }

    /// A user who already picked a theme in `railway ca` before this file
    /// existed must not see it reset when every screen starts reading the
    /// shared preference instead.
    #[test]
    fn seeds_from_agent_prefs_when_the_shared_file_does_not_exist_yet() {
        let home = tempfile::tempdir().unwrap();
        let railway_dir = home.path().join(".railway");
        std::fs::create_dir_all(&railway_dir).unwrap();
        std::fs::write(
            railway_dir.join("agent-prefs.json"),
            r#"{"version":1,"agent":"claude","theme":"ember"}"#,
        )
        .unwrap();

        assert_eq!(Theme::load_preference_in(home.path()).slug, "ember");
        // Seeded once, so the shared file now exists on its own.
        assert!(TuiPrefs::path_in(home.path()).exists());
    }

    #[test]
    fn a_typo_in_agent_prefs_theme_falls_back_to_default_without_seeding() {
        let home = tempfile::tempdir().unwrap();
        let railway_dir = home.path().join(".railway");
        std::fs::create_dir_all(&railway_dir).unwrap();
        std::fs::write(
            railway_dir.join("agent-prefs.json"),
            r#"{"version":1,"theme":"nonsense"}"#,
        )
        .unwrap();

        assert_eq!(Theme::load_preference_in(home.path()).slug, "railway");
    }

    /// Once the shared file exists, it is authoritative — cloud-agent's own
    /// file is never consulted again, even if the two disagree.
    #[test]
    fn the_shared_file_wins_once_it_exists() {
        let home = tempfile::tempdir().unwrap();
        let railway_dir = home.path().join(".railway");
        std::fs::create_dir_all(&railway_dir).unwrap();
        std::fs::write(
            railway_dir.join("agent-prefs.json"),
            r#"{"version":1,"theme":"ember"}"#,
        )
        .unwrap();
        Theme::from_slug(Some("mono"))
            .save_preference_in(home.path())
            .unwrap();

        assert_eq!(Theme::load_preference_in(home.path()).slug, "mono");
    }
}
