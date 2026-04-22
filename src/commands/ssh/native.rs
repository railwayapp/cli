use anyhow::{Context, Result, bail};
use is_terminal::IsTerminal;
use reqwest::Client;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::client::post_graphql;
use crate::config::Configs;
use crate::controllers::ssh_keys::{find_local_ssh_keys, register_ssh_key};
use crate::gql::queries::{ServiceInstance, service_instance};
use crate::util::prompt::{prompt_confirm_with_default, prompt_select};

const SSH_HOST: &str = "ssh.railway.com";

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

/// Ensure SSH key is registered, prompting user if needed
pub async fn ensure_ssh_key(client: &Client, configs: &Configs) -> Result<()> {
    let local_keys = find_local_ssh_keys()?;

    if local_keys.is_empty() {
        bail!(
            "No SSH keys found in ~/.ssh/\n\n\
            Generate one with:\n  ssh-keygen -t ed25519\n\n\
            Then run this command again."
        );
    }

    // Check which local keys are registered
    let registered_keys =
        crate::controllers::ssh_keys::get_registered_ssh_keys(client, configs).await?;

    // Find a local key that's already registered
    let registered_local = local_keys.iter().find(|local| {
        registered_keys
            .iter()
            .any(|r| r.fingerprint == local.fingerprint)
    });

    if let Some(key) = registered_local {
        eprintln!("Using SSH key: {}", key.path.display());
        return Ok(());
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
        struct KeyOption<'a>(&'a crate::controllers::ssh_keys::LocalSshKey);
        impl fmt::Display for KeyOption<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{} ({})", self.0.path.display(), self.0.fingerprint)
            }
        }
        let options: Vec<KeyOption> = local_keys.iter().map(KeyOption).collect();
        let selected = prompt_select("Which SSH key would you like to register?", options)?;
        selected.0
    };

    println!(
        "Key: {} ({})",
        key_to_register.path.display(),
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

    let key_name = key_to_register
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ssh-key")
        .to_string();

    register_ssh_key(client, configs, &key_name, &key_to_register.public_key).await?;

    println!("SSH key registered successfully!");

    Ok(())
}

/// Connect via SSH into a persistent tmux session, reconnecting on dropped connections.
/// Installs tmux in the container if it isn't already present.
pub fn run_native_ssh_with_session(ssh_target: &str, session_name: &str) -> Result<()> {
    let target = format!("{}@{}", ssh_target, SSH_HOST);

    // Ensure tmux is installed (suppress output — users don't need to see apt noise)
    eprintln!("Ensuring tmux is installed...");
    let install = Command::new("ssh")
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

    let tmux_cmd = format!(
        "exec tmux new-session -A -s {} \\; set -g mouse on",
        session_name
    );

    loop {
        let status = Command::new("ssh")
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

/// Run SSH command with the given service instance ID.
/// Optionally executes a command instead of starting an interactive shell.
pub fn run_native_ssh(service_instance_id: &str, command: Option<&[String]>) -> Result<i32> {
    let target = format!("{}@{}", service_instance_id, SSH_HOST);

    let mut ssh_cmd = Command::new("ssh");

    if command.is_some() {
        ssh_cmd.arg("-T");
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
