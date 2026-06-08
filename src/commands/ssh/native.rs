use anyhow::{Context, Result, bail};
use is_terminal::IsTerminal;
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::client::post_graphql;
use crate::config::Configs;
use crate::controllers::ssh::keys::{SshKeySource, find_local_ssh_keys, register_ssh_key};
use crate::gql::queries::{ServiceInstance, service_instance};
use crate::util::prompt::{prompt_confirm_with_default, prompt_select};

/// SSH relay endpoint (host, non-default port) for the current environment —
/// must track `Configs::get_backboard()`'s environment, or key registration
/// is checked against one backboard while the relay authenticates against
/// another (dev-mode CLIs used to dial the prod relay and get publickey
/// denials for keys that were registered fine).
pub(super) fn ssh_relay() -> (&'static str, Option<u16>) {
    Configs::get_ssh_relay()
}

/// Append `-p <port>` when the relay listens on a non-default port (the
/// develop relay uses 2222).
fn apply_relay_port(cmd: &mut Command, port: Option<u16>) {
    if let Some(port) = port {
        cmd.args(["-p", &port.to_string()]);
    }
}

/// Base `ssh` invocation for the current environment's relay: the binary, the
/// non-default relay port, and the `-i` identity when one was resolved.
/// Returns the command plus the `<target>@<relay-host>` to append *after* any
/// mode-specific options (interactive `-t`/`-T`, forward `-N`/`-L`, …). Shared
/// so the relay/port/identity setup can't drift between the interactive and
/// forward paths.
fn base_ssh_command(ssh_target: &str, identity_file: Option<&Path>) -> (Command, String) {
    let (host, port) = ssh_relay();
    let mut cmd = Command::new("ssh");
    apply_relay_port(&mut cmd, port);
    if let Some(key) = identity_file {
        cmd.arg("-i").arg(key);
    }
    (cmd, format!("{ssh_target}@{host}"))
}

/// Get the service instance ID for a service in an environment
pub async fn get_service_instance_id(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<String> {
    let vars = service_instance::Variables {
        environment_id: environment_id.to_string(),
        service_id: service_id.to_string(),
    };

    let response =
        post_graphql::<ServiceInstance, _>(client, configs.get_backboard(), vars).await?;

    Ok(response.service_instance.id)
}

/// Ensure SSH key is registered, prompting user if needed.
///
/// Queries/registers against whichever key bucket the backend picks from
/// the caller's auth context: a workspace-scoped `RAILWAY_API_TOKEN` gets
/// its workspace's keys; session and user tokens get personal keys. The
/// CLI doesn't need to distinguish — it passes `workspaceId: null` and
/// the resolver defaults from `ctx.workspace.id` when present.
pub async fn ensure_ssh_key(client: &Client, configs: &Configs) -> Result<Option<PathBuf>> {
    let local_keys = find_local_ssh_keys().await?;

    if local_keys.is_empty() {
        bail!(
            "No SSH keys found in your SSH agent or ~/.ssh/\n\n\
            Generate one with:\n  ssh-keygen -t ed25519\n\n\
            Then run this command again."
        );
    }

    let registered_keys =
        crate::controllers::ssh::keys::get_registered_ssh_keys(client, configs, None).await?;

    // Find a local key that's already registered
    let registered_local = local_keys.iter().find(|local| {
        registered_keys
            .iter()
            .any(|r| r.fingerprint == local.fingerprint)
    });

    if let Some(key) = registered_local {
        match &key.source {
            SshKeySource::File(path) => eprintln!(
                "Using SSH key from file {}: {}",
                path.display(),
                key.key_name()
            ),
            SshKeySource::Agent => eprintln!("Using SSH key from agent: {}", key.key_name()),
        }
        return Ok(identity_for(key));
    }

    // No local key is registered - need to register one
    if !std::io::stdin().is_terminal() {
        bail!(
            "No registered SSH keys found. Register one with:\n  railway ssh keys add\n\n\
            Or import from GitHub:\n  railway ssh keys github"
        );
    }

    println!("No SSH keys registered with Railway.");

    let key_to_register = if local_keys.len() == 1 {
        &local_keys[0]
    } else {
        // Let the user pick which key to register
        use std::fmt;
        struct KeyOption<'a>(&'a crate::controllers::ssh::keys::LocalSshKey);
        impl fmt::Display for KeyOption<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{} ({})", self.0.key_name(), self.0.fingerprint)
            }
        }
        let options: Vec<KeyOption> = local_keys.iter().map(KeyOption).collect();
        let selected = prompt_select("Which SSH key would you like to register?", options)?;
        selected.0
    };

    println!(
        "Key: {} ({})",
        key_to_register.key_name(),
        key_to_register.fingerprint
    );
    println!();

    let should_register = prompt_confirm_with_default("Register this SSH key with Railway?", true)?;

    if !should_register {
        bail!(
            "SSH key registration required for native SSH access.\n\
               You can also register your key at: https://railway.com/account/ssh-keys"
        );
    }

    register_ssh_key(
        client,
        configs,
        &key_to_register.key_name(),
        &key_to_register.public_key.to_string(),
        None,
    )
    .await?;

    println!("SSH key registered successfully!");

    Ok(identity_for(key_to_register))
}

/// The path to hand `ssh -i` for a registered local key. File-backed keys point
/// at the private key beside the `.pub`; agent-backed keys return `None` (the
/// agent offers them automatically, and there's no file to pass).
fn identity_for(key: &crate::controllers::ssh::keys::LocalSshKey) -> Option<PathBuf> {
    match &key.source {
        SshKeySource::File(path) => {
            if path.extension().and_then(|e| e.to_str()) == Some("pub") {
                Some(path.with_extension(""))
            } else {
                Some(path.to_path_buf())
            }
        }
        SshKeySource::Agent => None,
    }
}

/// Ensure tmux is installed inside the target container.
///
/// Split out from the session loop so that a tmux-install failure is
/// distinguishable from a session connect failure in telemetry.
pub fn ensure_tmux_installed(ssh_target: &str, identity_file: Option<&Path>) -> Result<()> {
    let (host, port) = ssh_relay();
    let target = format!("{ssh_target}@{host}");

    eprintln!("Ensuring tmux is installed...");
    let mut install_cmd = Command::new("ssh");
    apply_relay_port(&mut install_cmd, port);
    if let Some(key) = identity_file {
        install_cmd.arg("-i").arg(key);
    }
    let install = install_cmd
        .args(["-T", &target])
        .arg("which tmux || (apt-get update -qq && apt-get install -y -qq tmux)")
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to check/install tmux")?;

    if !install.success() {
        bail!("Failed to install tmux in the container");
    }

    Ok(())
}

/// Connect to a persistent tmux session, reconnecting on dropped connections.
/// Assumes `ensure_tmux_installed` has already succeeded.
///
/// Returns `Err` only if the local `ssh` binary fails to spawn. The loop
/// itself retries on any non-zero exit until the user disconnects cleanly,
/// which matches users' expectation that a tmux session survives flaps.
pub fn run_tmux_session(
    ssh_target: &str,
    session_name: &str,
    identity_file: Option<&Path>,
) -> Result<()> {
    let (host, port) = ssh_relay();
    let target = format!("{ssh_target}@{host}");
    let tmux_cmd = format!(
        "exec tmux new-session -A -s {} \\; set -g mouse on",
        session_name
    );

    loop {
        let mut session_cmd = Command::new("ssh");
        apply_relay_port(&mut session_cmd, port);
        if let Some(key) = identity_file {
            session_cmd.arg("-i").arg(key);
        }
        let status = session_cmd
            .args(["-t", &target, "--", &tmux_cmd])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("Failed to execute ssh command")?;

        match status.code().unwrap_or(255) {
            0 => break, // clean exit or detach — don't reconnect
            _ => {
                eprintln!("\r\nConnection lost. Reconnecting...");
                sleep(Duration::from_millis(500));
            }
        }
    }

    Ok(())
}

/// Resume request for a relay durable session, delivered via SSH `SetEnv`
/// (the relay intercepts these env keys; they are not forwarded to the VM).
pub struct DurableResume<'a> {
    pub session_name: &'a str,
    /// Resume from the server's last-read cursor instead of replaying the
    /// full retained scrollback.
    pub resume_from_last_read: bool,
}

/// Run SSH command with the given service instance ID.
/// Optionally executes a command instead of starting an interactive shell.
///
/// PTY allocation is autodetected from stdin/stdout TTY state, mirroring
/// the behavior of `docker exec` / `kubectl exec`:
///   - command + both TTYs  → `-t` (vim/htop work)
///   - command + non-TTY    → `-T` (clean pipes for scripts)
///   - no command + TTY     → ssh default (interactive shell with PTY)
///   - no command + non-TTY → `-T` (avoid mangling piped stdin)
pub fn run_native_ssh(
    service_instance_id: &str,
    command: Option<&[String]>,
    identity_file: Option<&Path>,
    durable: Option<DurableResume<'_>>,
) -> Result<i32> {
    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();

    let (mut ssh_cmd, target) = base_ssh_command(service_instance_id, identity_file);

    if let Some(durable) = durable {
        // Both env keys ride a single SetEnv directive: pre-8.7 OpenSSH only
        // honors the first SetEnv it encounters.
        let mut set_env = format!(
            "SetEnv RAILWAY_DURABLE_SESSION_NAME={}",
            durable.session_name
        );
        if durable.resume_from_last_read {
            set_env.push_str(" RAILWAY_DURABLE_RESUME=lastread");
        }
        ssh_cmd.arg("-o").arg(set_env);
    }

    match command {
        Some(_) if stdin_tty && stdout_tty => {
            ssh_cmd.arg("-t");
        }
        Some(_) => {
            ssh_cmd.arg("-T");
        }
        None if !stdin_tty => {
            ssh_cmd.arg("-T");
        }
        None => {}
    }

    ssh_cmd.arg(&target);

    if let Some(cmd_args) = command {
        for arg in cmd_args {
            ssh_cmd.arg(arg);
        }
    }

    ssh_cmd.stdin(Stdio::inherit());
    ssh_cmd.stdout(Stdio::inherit());
    ssh_cmd.stderr(Stdio::inherit());

    let status = ssh_cmd.status().context("Failed to execute ssh command")?;
    Ok(status.code().unwrap_or(1))
}

/// One `-L` style forward: localhost:`local_port` → 127.0.0.1:`remote_port`
/// inside the target.
#[derive(Clone)]
pub struct PortForward {
    pub local_port: u16,
    pub remote_port: u16,
}

/// Run a forward-only SSH session (`ssh -N -L ...`) against the relay.
///
/// The remote side is pinned to loopback — forwards reach ports the target
/// itself listens on, mirroring the `/ws/tcpip` bridge's behavior. Blocks
/// until the connection drops or the user interrupts; a signal-death (Ctrl+C)
/// is reported as exit code 0 since that's the normal way to stop a forward.
pub fn run_native_ssh_forward(
    ssh_target: &str,
    identity_file: Option<&Path>,
    forwards: &[PortForward],
) -> Result<i32> {
    let (mut ssh_cmd, target) = base_ssh_command(ssh_target, identity_file);

    ssh_cmd.args([
        "-N",
        // `-N` runs with stdin closed, so an interactive host-key prompt can't
        // be answered — a fresh machine that hasn't trusted the relay yet would
        // die with "Host key verification failed". accept-new auto-trusts on
        // first contact (TOFU) while still rejecting a *changed* key (MITM
        // protection). The interactive `sandbox ssh` path doesn't need this: it
        // inherits stdin and the user can type "yes".
        "-o",
        "StrictHostKeyChecking=accept-new",
        // Fail loudly if a forward can't be established instead of sitting
        // connected with nothing bound.
        "-o",
        "ExitOnForwardFailure=yes",
        // Long-lived idle forwards: detect a dead relay connection within
        // ~90s instead of hanging until the next local connection fails.
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ServerAliveCountMax=3",
    ]);

    for forward in forwards {
        ssh_cmd.args([
            "-L",
            &format!(
                "127.0.0.1:{}:127.0.0.1:{}",
                forward.local_port, forward.remote_port
            ),
        ]);
    }

    ssh_cmd.arg(&target);

    // `-N` runs no command; keep stderr inherited so relay/auth errors and
    // per-connection forward failures stay visible.
    ssh_cmd.stdin(Stdio::null());
    ssh_cmd.stdout(Stdio::null());
    ssh_cmd.stderr(Stdio::inherit());

    let status = ssh_cmd.status().context("Failed to execute ssh command")?;
    // `code()` is None when ssh died from a signal — for a forward that's the
    // user's Ctrl+C (delivered to the whole foreground process group).
    Ok(status.code().unwrap_or(0))
}
