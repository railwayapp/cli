use anyhow::{Result, anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;

use crate::client::{GQLClient, post_graphql};
use crate::commands::sandbox::{
    create_and_store, resolve_project_and_env, spawn_heartbeat, variables_to_input,
};
use crate::commands::ssh::{ensure_ssh_key, run_native_ssh, run_native_ssh_captured};
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

/// Sandbox-side seeds, safe to re-run:
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
const CODEX_SEED: &str = r#"umask 077
mkdir -p ~/.codex
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
fi"#;

/// The credential rides ssh stdin (never an argv) into a 0600 file.
const CODEX_INJECT_AUTH: &str = "umask 077; mkdir -p ~/.codex && cat > ~/.codex/auth.json";

/// Make sure the codex CLI exists in the sandbox; install it if not. Markers
/// on stdout (not exit codes) so a relay-level ssh failure is distinguishable
/// from "npm missing" / "install failed".
const CODEX_ENSURE_INSTALLED: &str = r#"if command -v codex >/dev/null 2>&1; then echo CODEX-READY; exit 0; fi
command -v npm >/dev/null 2>&1 || { echo CODEX-NO-NPM; exit 0; }
npm install -g @openai/codex >/dev/null 2>&1
if command -v codex >/dev/null 2>&1; then echo CODEX-READY; else echo CODEX-INSTALL-FAILED; fi"#;

fn codex_auth_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    Ok(home.join(".codex").join("auth.json"))
}

/// Plumbing ssh with transient-failure retries. A fresh sandbox can take a
/// beat before it accepts connections, and the relay can blip — those retry
/// with a short backoff. A host-key failure does NOT retry: it's a security
/// signal, so it fails immediately with the remediation instead of being
/// silently re-rolled. The remote scripts are idempotent, so re-running a
/// partially-applied attempt is safe.
fn ssh_plumbing(
    target: &str,
    command: &str,
    identity: Option<&std::path::Path>,
    stdin_payload: Option<&[u8]>,
) -> Result<Vec<u8>> {
    const ATTEMPTS: u32 = 3;
    let mut last = (1, String::new());
    for attempt in 1..=ATTEMPTS {
        let (code, out, err) = run_native_ssh_captured(target, command, identity, stdin_payload)?;
        if code == 0 {
            return Ok(out);
        }
        let err_text = String::from_utf8_lossy(&err).trim().to_string();
        if err_text.contains("Host key verification failed")
            || err_text.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        {
            bail!(
                "SSH host key verification failed for the Railway relay.\n\n{err_text}\n\nIf the relay's key legitimately rotated, refresh your entry with:\n  ssh-keygen -R ssh.railway.com && ssh-keyscan -t ed25519 ssh.railway.com >> ~/.ssh/known_hosts"
            );
        }
        last = (code, err_text);
        if attempt < ATTEMPTS {
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

    // --- Provision: seeds + credential (stdin) + make sure codex exists.
    {
        let target = target.clone();
        let identity = identity.clone();
        let mut spinner = create_shimmer_spinner("Provisioning codex");
        let provision = tokio::task::spawn_blocking(move || -> Result<()> {
            ssh_plumbing(&target, CODEX_SEED, identity.as_deref(), None)?;
            ssh_plumbing(
                &target,
                CODEX_INJECT_AUTH,
                identity.as_deref(),
                Some(&auth_bytes),
            )?;
            let out = ssh_plumbing(&target, CODEX_ENSURE_INSTALLED, identity.as_deref(), None)?;
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

    // --- Launch: interactive codex over the relay (a real PTY is allocated).
    let mut remote_cmd = String::from("cd ~ && exec codex");
    if !args.agent_args.is_empty() {
        remote_cmd.push(' ');
        remote_cmd.push_str(&shell_join(&args.agent_args));
    }

    println!("Launching codex…");
    let cmd = vec![remote_cmd];
    let exit_code = tokio::task::spawn_blocking(move || {
        run_native_ssh(&target, Some(&cmd), identity.as_deref(), None)
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
