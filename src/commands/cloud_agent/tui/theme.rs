//! Colour themes for the `railway ca` TUI.
//!
//! Every colour the TUI draws comes from here, so a theme is a value rather
//! than a set of scattered constants. Truecolor for the designed themes — the
//! relay already assumes a modern terminal, and Railway's violet has no ANSI-16
//! equivalent worth approximating — plus one theme built entirely from the
//! sixteen named colours, which is what to use when the terminal's own palette
//! should win.

use ratatui::style::Color;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    /// Stable identifier; persisted in `agent-prefs.json`.
    pub slug: &'static str,
    pub label: &'static str,
    /// Wordmark, selected rows, focused borders, key badges.
    pub accent: Color,
    /// Unfocused borders and hint lines.
    pub accent_dim: Color,
    /// Secondary text.
    pub dim: Color,
    /// Primary text.
    pub fg: Color,
    /// Text drawn on top of `accent` — must stay legible there.
    pub on_accent: Color,
    /// Row highlight in the tree.
    pub selection: Color,
    pub running: Color,
    pub sleeping: Color,
    pub pending: Color,
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
        selection: Color::Rgb(0x37, 0x2a, 0x50),
        running: Color::Rgb(0x6e, 0xe7, 0xa8),
        sleeping: Color::Rgb(0x7d, 0x77, 0x8f),
        pending: Color::Rgb(0xf5, 0xc0, 0x6b),
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
        selection: Color::DarkGray,
        running: Color::Green,
        sleeping: Color::Gray,
        pending: Color::Yellow,
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
        selection: Color::Rgb(0x45, 0x2d, 0x10),
        running: Color::Rgb(0x9a, 0xd8, 0x6a),
        sleeping: Color::Rgb(0x86, 0x7c, 0x6e),
        pending: Color::Rgb(0xf5, 0xd2, 0x6b),
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
        selection: Color::Rgb(0x33, 0x33, 0x33),
        running: Color::Rgb(0xe6, 0xe6, 0xe6),
        sleeping: Color::Rgb(0x77, 0x77, 0x77),
        pending: Color::Rgb(0xb4, 0xb4, 0xb4),
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
}
