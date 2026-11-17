//! What `create` and `ssh` print for sandboxes and cloud agents.
//!
//! A cloud agent is a sandbox with agent tooling on top, so both print the
//! same block in the same words and the same order. The agent adds one hint
//! line about coding agents; everything else is shared here so the two cannot
//! drift apart.

use colored::Colorize;

use crate::controllers::cloud_agent as ca;

/// Which kind of machine a message is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::commands) enum Kind {
    Sandbox,
    CloudAgent,
}

impl Kind {
    pub(in crate::commands) fn noun(self) -> &'static str {
        match self {
            Kind::Sandbox => "sandbox",
            Kind::CloudAgent => "cloud agent",
        }
    }
}

/// What happens to a machine nobody is using.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::commands) enum Idle {
    /// A sandbox, destroyed after this many idle minutes.
    DestroyedAfter(i64),
    /// A sandbox created with an idle timeout of 0.
    NeverIdlesOut,
    /// A cloud agent, put to sleep after this many idle minutes.
    SleepsAfter(i64),
    /// A cloud agent created before idle sleep shipped
    /// (see `ca::sleeps_when_idle`).
    NeverSleeps,
}

impl Idle {
    /// A sandbox's `idleTimeoutMinutes`: 0 (or less) never idles out, `None`
    /// is unknown.
    pub(in crate::commands) fn sandbox(minutes: Option<i64>) -> Option<Self> {
        minutes.map(|minutes| {
            if minutes <= 0 {
                Idle::NeverIdlesOut
            } else {
                Idle::DestroyedAfter(minutes)
            }
        })
    }

    /// A cloud agent's idle behavior, from when it was created.
    pub(in crate::commands) fn cloud_agent(created_at: chrono::DateTime<chrono::Utc>) -> Self {
        if ca::sleeps_when_idle(created_at) {
            Idle::SleepsAfter(ca::IDLE_SLEEP_MINUTES)
        } else {
            Idle::NeverSleeps
        }
    }

    fn field(self) -> String {
        match self {
            Idle::DestroyedAfter(minutes) => format!("Destroyed after {minutes} min idle"),
            Idle::NeverIdlesOut => "Never idles out".into(),
            Idle::SleepsAfter(minutes) => format!("Sleeps after {minutes} min idle"),
            Idle::NeverSleeps => "Never sleeps on its own".into(),
        }
    }
}

/// A public HTTP domain on the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::commands) struct Domain {
    pub domain: String,
    pub port: i64,
}

/// Everything the create block shows.
pub(in crate::commands) struct Created {
    pub kind: Kind,
    /// "Created" or "Forked".
    pub verb: &'static str,
    /// The name when the machine has one, otherwise its ID.
    pub label: String,
    /// The API's status, any casing ("RUNNING", "running").
    pub status: String,
    pub region: Option<String>,
    pub idle: Option<Idle>,
    pub domains: Vec<Domain>,
    /// Fewer domains came back than were requested: the rest are still being
    /// published.
    pub domains_publishing: bool,
    /// The command that connects to the machine.
    pub connect: String,
    /// One dim line under the connect line (the agent's coding-agent hint).
    pub hint: Option<String>,
}

const LABEL_WIDTH: usize = 9;

fn field(out: &mut String, label: &str, value: &str) {
    let padded = format!("{label:<LABEL_WIDTH$}");
    out.push_str(&format!("  {} {value}\n", padded.dimmed()));
}

/// "RUNNING" or "running" as "Running".
pub(in crate::commands) fn status_label(status: &str) -> String {
    let lower = status.to_lowercase().replace('_', " ");
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => lower,
    }
}

/// The create (or fork) block, with a trailing newline.
pub(in crate::commands) fn render_created(created: &Created) -> String {
    let mut out = format!(
        "✓ {} {} {}\n",
        created.verb,
        created.kind.noun(),
        created.label.cyan()
    );
    field(&mut out, "Status:", &status_label(&created.status));
    if let Some(region) = &created.region {
        field(&mut out, "Region:", region);
    }
    if let Some(idle) = created.idle {
        field(&mut out, "Idle:", &idle.field());
    }
    let mut domains: Vec<String> = created
        .domains
        .iter()
        .map(|d| format!("https://{} -> port {}", d.domain, d.port))
        .collect();
    if created.domains_publishing {
        domains.insert(
            0,
            match created.kind {
                Kind::Sandbox => "Publishing (`railway sandbox list` shows the URLs)".into(),
                Kind::CloudAgent => "Publishing".into(),
            },
        );
    }
    for (i, domain) in domains.iter().enumerate() {
        field(&mut out, if i == 0 { "Domains:" } else { "" }, domain);
    }
    out.push_str(&format!("\nConnect: {}\n", created.connect.cyan()));
    if let Some(hint) = &created.hint {
        out.push_str(&format!("{}\n", hint.dimmed()));
    }
    out
}

/// The line printed after an interactive session ends:
/// `Disconnected from <kind> <label>. <what happens next>. <act now>.`
///
/// `label` is shown as given (possibly colored); `name` is the plain handle
/// used in commands. Only the agent's commands take a name: `railway sandbox
/// ssh` makes the sandbox active, so `railway sandbox destroy` targets it.
pub(in crate::commands) fn disconnected(
    kind: Kind,
    label: &str,
    name: &str,
    idle: Option<Idle>,
) -> String {
    let next = match (kind, idle) {
        (_, Some(Idle::DestroyedAfter(minutes))) => format!(
            "It's destroyed after {minutes} min idle. `railway sandbox destroy` removes it now."
        ),
        (_, Some(Idle::NeverIdlesOut)) => {
            "It keeps running until you destroy it with `railway sandbox destroy`.".into()
        }
        (Kind::Sandbox, _) => {
            "It's destroyed once it sits idle. `railway sandbox destroy` removes it now.".into()
        }
        (Kind::CloudAgent, Some(Idle::NeverSleeps)) => {
            format!("It keeps running until you sleep it with `railway ca sleep {name}`.")
        }
        (Kind::CloudAgent, _) => format!(
            "It {}. `railway ca sleep {name}` sleeps it now.",
            ca::IDLE_SLEEP_CONDITIONS
        ),
    };
    format!("Disconnected from {} {label}. {next}", kind.noun())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: String) -> String {
        // The renderer colors labels; tests read the words.
        console::strip_ansi_codes(&text).into_owned()
    }

    fn sandbox(status: &str, idle: Option<i64>, domains: Vec<Domain>, requested: usize) -> Created {
        Created {
            kind: Kind::Sandbox,
            verb: "Created",
            label: "4f9d2c1e-7b3a-4c8e-9a51-2d6f0b8e3c47".into(),
            status: status.into(),
            region: Some("us-west2".into()),
            idle: Idle::sandbox(idle),
            domains_publishing: domains.len() < requested,
            domains,
            connect: "railway sandbox ssh".into(),
            hint: None,
        }
    }

    fn agent(status: &str, region: Option<&str>, domains: Vec<Domain>) -> Created {
        Created {
            kind: Kind::CloudAgent,
            verb: "Created",
            label: "my-box".into(),
            status: status.into(),
            region: region.map(Into::into),
            idle: Some(Idle::SleepsAfter(5)),
            domains,
            domains_publishing: false,
            connect: "railway ca ssh my-box".into(),
            hint: Some(
                "No coding agent is installed yet. `railway ca ssh my-box --session` sets one up."
                    .into(),
            ),
        }
    }

    #[test]
    fn sandbox_with_domains() {
        let out = plain(render_created(&sandbox(
            "RUNNING",
            Some(10),
            vec![
                Domain {
                    domain: "web-8f2a.up.railway.app".into(),
                    port: 3000,
                },
                Domain {
                    domain: "api-8f2a.up.railway.app".into(),
                    port: 8080,
                },
            ],
            2,
        )));
        assert_eq!(
            out,
            "\
✓ Created sandbox 4f9d2c1e-7b3a-4c8e-9a51-2d6f0b8e3c47
  Status:   Running
  Region:   us-west2
  Idle:     Destroyed after 10 min idle
  Domains:  https://web-8f2a.up.railway.app -> port 3000
            https://api-8f2a.up.railway.app -> port 8080

Connect: railway sandbox ssh
"
        );
    }

    #[test]
    fn sandbox_without_domains() {
        let out = plain(render_created(&sandbox("RUNNING", Some(10), vec![], 0)));
        assert_eq!(
            out,
            "\
✓ Created sandbox 4f9d2c1e-7b3a-4c8e-9a51-2d6f0b8e3c47
  Status:   Running
  Region:   us-west2
  Idle:     Destroyed after 10 min idle

Connect: railway sandbox ssh
"
        );
    }

    #[test]
    fn never_idle_sandbox() {
        let out = plain(render_created(&sandbox("RUNNING", Some(0), vec![], 0)));
        assert!(out.contains("  Idle:     Never idles out\n"), "{out}");
        // Unknown idle is left out rather than guessed.
        let out = plain(render_created(&sandbox("RUNNING", None, vec![], 0)));
        assert!(!out.contains("Idle:"), "{out}");
    }

    #[test]
    fn no_wait_sandbox_shows_its_current_status_and_publishing_domains() {
        let out = plain(render_created(&sandbox("CREATING", Some(10), vec![], 1)));
        assert_eq!(
            out,
            "\
✓ Created sandbox 4f9d2c1e-7b3a-4c8e-9a51-2d6f0b8e3c47
  Status:   Creating
  Region:   us-west2
  Idle:     Destroyed after 10 min idle
  Domains:  Publishing (`railway sandbox list` shows the URLs)

Connect: railway sandbox ssh
"
        );
    }

    #[test]
    fn forked_sandbox_says_forked() {
        let mut fork = sandbox("RUNNING", Some(10), vec![], 0);
        fork.verb = "Forked";
        let out = plain(render_created(&fork));
        assert!(out.starts_with("✓ Forked sandbox 4f9d2c1e-"), "{out}");
    }

    #[test]
    fn cloud_agent() {
        let out = plain(render_created(&agent(
            "running",
            Some("us-east4"),
            vec![Domain {
                domain: "app-3c9e1f.up.railway.app".into(),
                port: 8080,
            }],
        )));
        assert_eq!(
            out,
            "\
✓ Created cloud agent my-box
  Status:   Running
  Region:   us-east4
  Idle:     Sleeps after 5 min idle
  Domains:  https://app-3c9e1f.up.railway.app -> port 8080

Connect: railway ca ssh my-box
No coding agent is installed yet. `railway ca ssh my-box --session` sets one up.
"
        );
    }

    #[test]
    fn no_wait_cloud_agent_shows_its_current_status() {
        // Not placed yet: no region, no domains, and no rows for them.
        let out = plain(render_created(&agent("STARTING", None, vec![])));
        assert_eq!(
            out,
            "\
✓ Created cloud agent my-box
  Status:   Starting
  Idle:     Sleeps after 5 min idle

Connect: railway ca ssh my-box
No coding agent is installed yet. `railway ca ssh my-box --session` sets one up.
"
        );
    }

    #[test]
    fn both_kinds_share_the_block_structure() {
        let shape = |out: String| -> Vec<String> {
            plain(out)
                .lines()
                .map(|line| line.split(':').next().unwrap_or_default().to_owned())
                .collect()
        };
        let sandbox = shape(render_created(&sandbox(
            "RUNNING",
            Some(10),
            vec![Domain {
                domain: "a".into(),
                port: 1,
            }],
            1,
        )));
        let agent = shape(render_created(&agent(
            "running",
            Some("r"),
            vec![Domain {
                domain: "a".into(),
                port: 1,
            }],
        )));
        // The agent adds only its hint line.
        assert_eq!(sandbox[1..], agent[1..sandbox.len()]);
        assert_eq!(agent.len(), sandbox.len() + 1);
    }

    #[test]
    fn status_labels_are_sentence_case() {
        assert_eq!(status_label("RUNNING"), "Running");
        assert_eq!(status_label("running"), "Running");
        assert_eq!(status_label("NEW_STATE"), "New state");
        assert_eq!(status_label(""), "");
    }

    #[test]
    fn sandbox_disconnect_lines() {
        assert_eq!(
            disconnected(Kind::Sandbox, "sbx", "sbx", Idle::sandbox(Some(10))),
            "Disconnected from sandbox sbx. It's destroyed after 10 min idle. `railway sandbox destroy` removes it now."
        );
        assert_eq!(
            disconnected(Kind::Sandbox, "sbx", "sbx", Idle::sandbox(Some(0))),
            "Disconnected from sandbox sbx. It keeps running until you destroy it with `railway sandbox destroy`."
        );
        assert_eq!(
            disconnected(Kind::Sandbox, "sbx", "sbx", None),
            "Disconnected from sandbox sbx. It's destroyed once it sits idle. `railway sandbox destroy` removes it now."
        );
    }

    #[test]
    fn cloud_agent_disconnect_lines_follow_the_idle_sleep_cutoff() {
        let at = |s: &str| s.parse::<chrono::DateTime<chrono::Utc>>().unwrap();
        let new = Idle::cloud_agent(at("2026-11-01T00:00:00Z"));
        let old = Idle::cloud_agent(at("2026-09-01T00:00:00Z"));
        assert_eq!(new, Idle::SleepsAfter(5));
        assert_eq!(old, Idle::NeverSleeps);
        assert_eq!(
            disconnected(Kind::CloudAgent, "my-box", "my-box", Some(new)),
            "Disconnected from cloud agent my-box. It sleeps after 5 minutes with no SSH client, network traffic or agent turn, which ends its processes. `railway ca sleep my-box` sleeps it now."
        );
        assert_eq!(
            disconnected(Kind::CloudAgent, "my-box", "my-box", Some(old)),
            "Disconnected from cloud agent my-box. It keeps running until you sleep it with `railway ca sleep my-box`."
        );
    }
}
