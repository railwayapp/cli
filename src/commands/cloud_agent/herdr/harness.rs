//! Which coding agent a VM is provisioned for: flags first, then a pick that
//! defaults to `railway ca setup`'s choice.

use std::fmt;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

use crate::commands::code;
use crate::config::Configs;

#[derive(ClapArgs, Default, Clone)]
pub struct HarnessFlags {
    /// Provision for Claude Code
    #[clap(long, conflicts_with_all = ["codex", "grok", "railway"])]
    pub claude: bool,

    /// Provision for OpenAI Codex
    #[clap(long, conflicts_with_all = ["grok", "railway"])]
    pub codex: bool,

    /// Provision for xAI Grok
    #[clap(long, conflicts_with = "railway")]
    pub grok: bool,

    /// Provision for Railway's own agent (no sign-in needed)
    #[clap(long)]
    pub railway: bool,
}

impl HarnessFlags {
    pub fn slug(&self) -> Option<&'static str> {
        [
            (self.claude, "claude"),
            (self.codex, "codex"),
            (self.grok, "grok"),
            (self.railway, "railway"),
        ]
        .into_iter()
        .find_map(|(on, slug)| on.then_some(slug))
    }
}

const CHOICES: [(&str, &str); 4] = [
    ("claude", "Anthropic's Claude Code"),
    ("codex", "OpenAI's Codex"),
    ("grok", "xAI's Grok"),
    ("railway", "Railway's own agent, no sign-in needed"),
];

struct Choice(&'static str, &'static str);

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:<8} {}", self.0, self.1)
    }
}

/// A flag wins; otherwise ask (starting on the saved default) or, when asking
/// is not wanted, take the saved default.
pub fn choose(flags: &HarnessFlags, ask: bool) -> Result<&'static str> {
    if let Some(slug) = flags.slug() {
        return Ok(slug);
    }
    let default = code::default_harness()?;
    if !ask {
        return Ok(default);
    }
    let options: Vec<Choice> = CHOICES.iter().map(|(s, b)| Choice(s, b)).collect();
    let picked = inquire::Select::new("Coding agent", options)
        .with_starting_cursor(default_cursor(default))
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for the coding agent")?;
    Ok(picked.0)
}

fn default_cursor(default: &str) -> usize {
    CHOICES
        .iter()
        .position(|(slug, _)| *slug == default)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_maps_to_its_slug() {
        let flags = HarnessFlags {
            codex: true,
            ..Default::default()
        };
        assert_eq!(flags.slug(), Some("codex"));
        assert_eq!(HarnessFlags::default().slug(), None);
    }

    #[test]
    fn the_saved_default_is_the_starting_row() {
        assert_eq!(default_cursor("grok"), 2);
        assert_eq!(default_cursor("shell"), 0);
    }
}
