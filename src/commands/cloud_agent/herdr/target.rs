//! The two strings herdr sees for an agent: its ssh target and its label.

use crate::config::Configs;
use crate::controllers::cloud_agent as ca;

/// herdr hands the target straight to `ssh`, and the relay reads
/// `agent:<env>:<id>` as the username. herdr rejects a literal `:` in the
/// user part as a password, so the colons ride percent-encoded in the URI
/// form, which OpenSSH decodes (verified against the relay).
pub fn target(agent: &ca::Agent) -> String {
    target_for(&agent.environment_id, &agent.id)
}

pub fn target_for(environment_id: &str, agent_id: &str) -> String {
    let (host, port) = Configs::get_ssh_relay();
    let user = format!("agent%3A{environment_id}%3A{agent_id}");
    match port {
        Some(port) => format!("ssh://{user}@{host}:{port}"),
        None => format!("ssh://{user}@{host}"),
    }
}

/// The agent id inside a target this module wrote, `None` for anyone else's.
pub fn agent_id_of(target: &str) -> Option<String> {
    let user = target.strip_prefix("ssh://").unwrap_or(target);
    let user = user.rsplit_once('@')?.0;
    let user = user.replace("%3A", ":").replace("%3a", ":");
    let mut parts = user.splitn(3, ':');
    if parts.next()? != "agent" {
        return None;
    }
    parts.next()?;
    parts.next().map(str::to_owned)
}

/// Sidebar budget. herdr clips labels rather than wrapping, and the header row
/// also carries the connection signal, so a 26-column sidebar shows about 24
/// characters of label. Project first for grouping; the agent name is the part
/// you search for, so it keeps its characters and the project absorbs the cut.
pub const LABEL_WIDTH: usize = 24;

pub fn label(project: &str, name: &str) -> String {
    label_within(project, name, LABEL_WIDTH)
}

fn label_within(project: &str, name: &str, width: usize) -> String {
    let project = project.trim();
    let name = name.trim();
    let plen = project.chars().count();
    let nlen = name.chars().count();
    if plen + 1 + nlen <= width {
        return format!("{project}/{name}");
    }
    const PROJECT_FLOOR: usize = 6;
    let name = cut(name, (width - 1).saturating_sub(PROJECT_FLOOR).max(1));
    let project = cut(
        project,
        (width - 1).saturating_sub(name.chars().count()).max(1),
    );
    format!("{project}/{name}")
}

fn cut(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_labels_are_untouched() {
        assert_eq!(label("orch", "reviewer"), "orch/reviewer");
    }

    #[test]
    fn long_pairs_fit_the_budget_with_project_cut_first() {
        let l = label("railway-sandboxes", "content-wonder");
        assert!(l.chars().count() <= LABEL_WIDTH, "{l}");
        assert!(l.ends_with("/content-wonder"), "{l}");
        assert!(l.starts_with("railway-"), "{l}");
    }

    #[test]
    fn both_halves_are_cut_when_both_are_long() {
        let l = label("Hello World Page", "Build a simple hello world");
        assert!(l.chars().count() <= LABEL_WIDTH, "{l}");
        assert!(l.contains('…'), "{l}");
        assert!(l.contains('/'), "{l}");
    }

    #[test]
    fn target_round_trips_the_agent_id() {
        let t = target_for("env-1", "agent-1");
        assert!(t.starts_with("ssh://agent%3Aenv-1%3Aagent-1@"), "{t}");
        let userinfo = t.trim_start_matches("ssh://").rsplit_once('@').unwrap().0;
        assert!(!userinfo.contains(':'), "{t}");
        assert_eq!(agent_id_of(&t).as_deref(), Some("agent-1"));
        assert_eq!(
            agent_id_of("agent:env-1:agent-1@ssh.railway.com").as_deref(),
            Some("agent-1")
        );
        assert_eq!(agent_id_of("workbox"), None);
        assert_eq!(agent_id_of("me@workbox"), None);
    }
}
