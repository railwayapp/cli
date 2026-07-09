use anyhow::{Result, anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;

use crate::client::{GQLClient, post_graphql};
use crate::commands::sandbox::{
    create_and_store, resolve_project_and_env, spawn_heartbeat, variables_to_input,
};
use crate::commands::ssh::{ensure_ssh_key, run_native_ssh_captured, run_native_ssh_with_opts};
use crate::config::Configs;
use crate::gql::{mutations, queries};
use crate::util::progress::{create_shimmer_spinner, fail_spinner};
use crate::util::prompt::prompt_confirm_with_default_with_cancel;
use crate::util::shell::shell_join;

// ---------------------------------------------------------------------------
// `railway code --codex` — launch OpenAI Codex in a Railway sandbox on the
// user's own ChatGPT plan.
//
// Auth shape: the user's existing local sign-in (`~/.codex/auth.json`) is
// copied into the sandbox — the same flow OpenAI documents for remote
// machines and containers (developers.openai.com/codex/auth). The copy is
// consent-gated, read client-side, and rides ssh stdin into a 0600 file in
// the sandbox: it never appears in an argv, a Railway variable, an image, or
// server-side config. Nothing is stored locally by this command.
// ---------------------------------------------------------------------------

/// Launch a coding agent in a Railway sandbox
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway code --codex              # sandbox + your local Codex sign-in\n  railway code --codex --new        # force a fresh sandbox\n  railway code --codex -- exec \"explain this codebase\"\n\nNote: requires the PROJECT_SANDBOXES feature to be enabled."
)]
pub struct Args {
    /// Launch OpenAI Codex using your local ChatGPT sign-in (~/.codex/auth.json)
    #[clap(long)]
    codex: bool,

    /// Always create a fresh sandbox instead of reusing the active one
    #[clap(long)]
    new: bool,

    /// Skip the confirmation prompt before reading local agent credentials
    #[clap(long, short = 'y')]
    yes: bool,

    /// Minutes the sandbox may sit idle (disconnected) before it is
    /// auto-destroyed
    #[clap(long, value_name = "MINUTES", default_value = "30")]
    idle_timeout: i64,

    /// Environment name or ID (defaults to the linked environment)
    #[clap(long, short)]
    environment: Option<String>,

    /// Project ID (defaults to the linked project)
    #[clap(long, short)]
    project: Option<String>,

    /// Extra arguments passed through to the agent (after `--`)
    #[clap(trailing_var_arg = true)]
    agent_args: Vec<String>,
}

/// The whole sandbox-side provision as ONE script over ONE connection — the
/// credential arrives on stdin (never an argv) into a 0600 file, then the
/// seeds and the codex install run. One connection instead of three matters:
/// success/failure markers ride stdout so a relay-level failure is
/// distinguishable from "npm missing" / "install failed".
///
/// Seeds, all idempotent:
/// - COLORTERM: the relay forwards TERM but not COLORTERM; without it TUIs
///   render a greyed/degraded palette.
/// - config.toml: pre-trust the dirs codex lands in ($HOME via `cd ~`, and /
///   where the relay drops non-cd sessions) or the TUI stops at a folder-trust
///   prompt even when authenticated. Only seeded when no config exists, so a
///   user-customized config is never clobbered.
/// - ~/.profile autostart: plain connects (`railway sandbox ssh`) run bash as
///   a login shell (verified: ~/.profile IS sourced; command sessions are
///   not), so any interactive reconnect drops into codex. Not `exec`, so
///   quitting codex lands in a shell instead of closing the connection. The
///   `[ -t 1 ]` guard keeps scp-style and command sessions out.
const CODEX_PROVISION: &str = r#"umask 077
mkdir -p ~/.codex
cat > ~/.codex/auth.json
grep -q "^COLORTERM=" /etc/environment 2>/dev/null || echo "COLORTERM=truecolor" >> /etc/environment 2>/dev/null || true
if [ ! -f ~/.codex/config.toml ]; then
cat > ~/.codex/config.toml <<'EOF'
[projects."/root"]
trust_level = "trusted"

[projects."/"]
trust_level = "trusted"
EOF
fi
if ! grep -q "railway-code codex autostart" ~/.profile 2>/dev/null; then
cat >> ~/.profile <<'PROFEOF'

# railway-code codex autostart (connecting drops into codex; exit it for a shell)
if [ -z "$RAILWAY_CODE_AUTOSTARTED" ] && [ -t 1 ] && command -v codex >/dev/null 2>&1; then
  export RAILWAY_CODE_AUTOSTARTED=1
  cd "$HOME" && codex
fi
PROFEOF
fi
if command -v codex >/dev/null 2>&1; then echo CODEX-READY; exit 0; fi
command -v npm >/dev/null 2>&1 || { echo CODEX-NO-NPM; exit 0; }
npm install -g @openai/codex >/dev/null 2>&1
if command -v codex >/dev/null 2>&1; then echo CODEX-READY; else echo CODEX-INSTALL-FAILED; fi"#;

/// SSH options shared by every connection this command runs, plus the info
/// needed to self-heal our relay known-hosts file. Two layers:
///
/// **Multiplexing** — the relay fleet answers with per-instance host keys, so
/// each fresh TCP connection is a new host-key roll; a multiplexed session
/// rides the master's already-verified connection. One `railway code` run
/// makes exactly one host-key decision instead of one per step.
/// ControlPersist keeps the master alive briefly so the interactive launch
/// reuses the provisioning master.
///
/// **Dedicated known-hosts** — the fleet currently presents many distinct
/// per-instance keys behind one hostname (7+ observed), so pinning a single
/// key is both futile (most connections mismatch) and security theater (a
/// fresh TOFU accept is indistinguishable from a MITM anyway). Relay
/// connections from this command therefore verify against the CLI's own
/// file (`~/.railway/known_hosts_relay`) with accept-new, leaving the user's
/// ~/.ssh/known_hosts untouched, and `ssh_plumbing` may heal THIS file (and
/// only this file) on a mismatch. Revisit when the relay ships a stable
/// shared host key or CA: flip to strict checking against the published key.
#[derive(Clone)]
struct RelaySsh {
    opts: Vec<String>,
    known_hosts: std::path::PathBuf,
    /// known-hosts pattern for ssh-keygen -R: `host` or `[host]:port`.
    host_pattern: String,
}

fn relay_ssh() -> Result<RelaySsh> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    let ssh_dir = home.join(".ssh");
    if !ssh_dir.exists() {
        std::fs::create_dir_all(&ssh_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let railway_dir = home.join(".railway");
    std::fs::create_dir_all(&railway_dir)?;
    let known_hosts = railway_dir.join("known_hosts_relay");

    let (host, port) = Configs::get_ssh_relay();
    let host_pattern = match port {
        Some(p) if p != 22 => format!("[{host}]:{p}"),
        _ => host.to_string(),
    };

    // %C hashes (local host, remote user, host, port) — short & per-target,
    // safely under the unix socket path length limit.
    let control_path = ssh_dir.join("railway-cm-%C");
    Ok(RelaySsh {
        opts: vec![
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={}", control_path.display()),
            "-o".into(),
            "ControlPersist=90s".into(),
            "-o".into(),
            format!("UserKnownHostsFile={}", known_hosts.display()),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
        ],
        known_hosts,
        host_pattern,
    })
}

impl RelaySsh {
    /// Drop the relay's entry from OUR known-hosts file so the next attempt
    /// re-accepts whichever fleet key answers. Never touches ~/.ssh.
    fn heal_known_hosts(&self) {
        let _ = std::process::Command::new("ssh-keygen")
            .arg("-R")
            .arg(&self.host_pattern)
            .arg("-f")
            .arg(&self.known_hosts)
            .output();
    }
}

fn codex_auth_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    Ok(home.join(".codex").join("auth.json"))
}

/// Plumbing ssh with retries. A fresh sandbox can take a beat before it
/// accepts connections, and the relay can blip — those retry with a short
/// backoff. A host-key mismatch heals the CLI's OWN relay known-hosts file
/// (the fleet presents per-instance keys, so a mismatch there is expected,
/// not a signal — see `relay_ssh`) and retries; the user's ~/.ssh files are
/// never modified. The remote scripts are idempotent, so re-running a
/// partially-applied attempt is safe.
fn ssh_plumbing(
    target: &str,
    command: &str,
    identity: Option<&std::path::Path>,
    stdin_payload: Option<&[u8]>,
    relay: &RelaySsh,
) -> Result<Vec<u8>> {
    const ATTEMPTS: u32 = 4;
    let mut last = (1, String::new());
    for attempt in 1..=ATTEMPTS {
        let (code, out, err) =
            run_native_ssh_captured(target, command, identity, stdin_payload, &relay.opts)?;
        if code == 0 {
            return Ok(out);
        }
        let err_text = String::from_utf8_lossy(&err).trim().to_string();
        let hostkey_mismatch = err_text.contains("Host key verification failed")
            || err_text.contains("REMOTE HOST IDENTIFICATION HAS CHANGED");
        if hostkey_mismatch {
            relay.heal_known_hosts();
        }
        last = (code, err_text);
        if attempt < ATTEMPTS && !hostkey_mismatch {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    let (code, err_text) = last;
    if err_text.is_empty() {
        bail!("SSH to the sandbox failed after {ATTEMPTS} attempts (exit {code}).")
    }
    bail!("SSH to the sandbox failed after {ATTEMPTS} attempts (exit {code}):\n{err_text}")
}

pub async fn command(args: Args) -> Result<()> {
    use colored::Colorize;

    if !args.codex {
        bail!(
            "Specify which agent to launch, e.g.:\n  railway code --codex\n\n(more agents coming soon)"
        );
    }

    eprintln!(
        "{}",
        "Warning: Railway sandboxes are experimental and APIs may change or break during testing."
            .yellow()
    );

    // --- Resolve the local Codex credential (client-side only, consent-gated).
    let auth_path = codex_auth_path()?;
    if !auth_path.exists() {
        bail!(
            "No Codex sign-in found at {}.\nRun `codex login` locally first (or `codex login --device-auth` on this machine), then re-run this command.",
            auth_path.display()
        );
    }

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if !args.yes {
        if !interactive {
            bail!(
                "Refusing to read {} without confirmation in a non-interactive session. Pass --yes to consent.",
                auth_path.display()
            );
        }
        let msg = format!(
            "Copy your local Codex sign-in ({}) into this sandbox?",
            auth_path.display()
        );
        match prompt_confirm_with_default_with_cancel(&msg, true)? {
            Some(true) => {}
            _ => bail!(
                "Aborted — nothing was read. Tip: you can instead sign in inside a sandbox with `codex login --device-auth`."
            ),
        }
    }
    let auth_bytes = std::fs::read(&auth_path)?;
    if auth_bytes.is_empty() {
        bail!("{} is empty — run `codex login` locally first.", auth_path.display());
    }

    // --- Resolve where the sandbox lives.
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (project_id, environment_id) =
        resolve_project_and_env(&mut configs, &client, args.project, args.environment).await?;

    // Reuse the active sandbox when it's still alive here — repeated runs
    // shouldn't mint a fleet of boxes. CREATING counts as alive: a re-run
    // seconds after a launch (flaky connection, ctrl-c) must reuse the box
    // that's still booting, not spawn a duplicate. `--new` forces a fresh one.
    let reusable = if args.new {
        None
    } else {
        match configs.get_active_sandbox() {
            Some(stored) if stored.environment_id == environment_id => {
                let res = post_graphql::<queries::Sandboxes, _>(
                    &client,
                    configs.get_backboard(),
                    queries::sandboxes::Variables {
                        environment_id: environment_id.clone(),
                        first: Some(100),
                        after: None,
                    },
                )
                .await?;
                res.sandboxes
                    .edges
                    .into_iter()
                    .map(|e| e.node)
                    .find(|n| {
                        n.id == stored.id
                            && matches!(
                                n.status,
                                queries::sandboxes::SandboxStatus::RUNNING
                                    | queries::sandboxes::SandboxStatus::CREATING
                            )
                    })
                    .map(|n| n.id)
            }
            _ => None,
        }
    };

    let sandbox_id = if let Some(id) = reusable {
        println!("Reusing active sandbox {id} (use --new for a fresh one)");
        id
    } else {
        let input = mutations::sandbox_create::SandboxCreateInput {
            environment_id: environment_id.clone(),
            // Default is a shorter idle window for a credential-bearing box
            // than the CLI default; it's interactive, so this only reaps
            // forgotten ones. `--idle-timeout` overrides.
            idle_timeout_minutes: Some(args.idle_timeout),
            template: None,
            source_sandbox_id: None,
            network_isolation: None,
            variables: variables_to_input(&[], &[])?,
        };
        create_and_store(
            &mut configs,
            &client,
            project_id.clone(),
            environment_id.clone(),
            input,
            false,
            false,
        )
        .await?
    };

    let identity = ensure_ssh_key(&client, &configs).await?;
    let target = format!("sbx:{environment_id}:{sandbox_id}");
    let heartbeat = spawn_heartbeat(
        client.clone(),
        configs.get_backboard(),
        environment_id.clone(),
        sandbox_id.clone(),
    );

    // Multiplex every ssh in this run over one verified connection: the
    // provisioning call establishes the master, the interactive launch rides
    // it — one host-key decision per run, not one per connection.
    let relay = relay_ssh()?;

    // --- Provision: credential (stdin) + seeds + codex install, one script.
    {
        let target = target.clone();
        let identity = identity.clone();
        let relay = relay.clone();
        let mut spinner = create_shimmer_spinner("Provisioning codex");
        let provision = tokio::task::spawn_blocking(move || -> Result<()> {
            let out = ssh_plumbing(
                &target,
                CODEX_PROVISION,
                identity.as_deref(),
                Some(&auth_bytes),
                &relay,
            )?;
            let out = String::from_utf8_lossy(&out);
            if out.contains("CODEX-READY") {
                Ok(())
            } else if out.contains("CODEX-NO-NPM") {
                bail!("The sandbox image has no npm, so codex can't be installed automatically.")
            } else if out.contains("CODEX-INSTALL-FAILED") {
                bail!("`npm install -g @openai/codex` failed in the sandbox.")
            } else {
                bail!("Provisioning produced no status marker — the connection likely dropped mid-script.")
            }
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
        match provision {
            Ok(()) => spinner.finish_and_clear(),
            Err(e) => {
                fail_spinner(&mut spinner, "Provisioning failed".to_string());
                heartbeat.abort();
                return Err(e);
            }
        }
    }

    // --- Launch: interactive codex over the relay (a real PTY is allocated),
    // multiplexed over the provisioning master.
    let mut remote_cmd = String::from("cd ~ && exec codex");
    if !args.agent_args.is_empty() {
        remote_cmd.push(' ');
        remote_cmd.push_str(&shell_join(&args.agent_args));
    }

    println!("Launching codex…");
    let cmd = vec![remote_cmd];
    let exit_code = tokio::task::spawn_blocking(move || {
        run_native_ssh_with_opts(&target, Some(&cmd), identity.as_deref(), None, &relay.opts)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)?;

    heartbeat.abort();

    // Where-did-my-sandbox-go breadcrumbs: the box outlives the session but
    // only for the idle window, and `sandbox list` is environment-scoped —
    // spell out the commands that find it from anywhere.
    println!(
        "\nDisconnected — sandbox {sandbox_id} stays up for ~{}m of idle time.",
        args.idle_timeout
    );
    println!("Get back in:");
    println!("  railway sandbox ssh      # drops straight back into codex");
    println!("  railway code --codex     # same, from any dir linked to this project");
    println!("Find it later (sandbox list is per-environment):");
    println!("  railway sandbox list -p {project_id} -e {environment_id}");

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}
