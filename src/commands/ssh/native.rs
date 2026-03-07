use anyhow::{Result, bail, Context};
use is_terminal::IsTerminal;
use reqwest::Client;
use std::process::{Command, Stdio};

use crate::client::post_graphql;
use crate::config::Configs;
use crate::controllers::ssh_keys::{
    ensure_ssh_key_registered, find_local_ssh_keys, register_ssh_key,
};
use crate::gql::queries::{service_instance, ServiceInstance};
use crate::util::prompt::prompt_confirm_with_default;

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
    };

    let response =
        post_graphql::<ServiceInstance, _>(client, configs.get_backboard(), vars).await?;

    // Find the service instance matching our service
    for edge in response.environment.service_instances.edges {
        if edge.node.service_id == service_id {
            return Ok(edge.node.id);
        }
    }

    bail!("No service instance found for service {} in environment {}", service_id, environment_id)
}

/// Ensure SSH key is registered, prompting user if needed
pub async fn ensure_ssh_key(client: &Client, configs: &Configs) -> Result<()> {
    let local_key = ensure_ssh_key_registered(client, configs).await?;

    // Check if this key is already registered (the function returns the key even if not registered)
    let registered_keys = crate::controllers::ssh_keys::get_registered_ssh_keys(client, configs).await?;
    let is_registered = registered_keys.iter().any(|k| k.fingerprint == local_key.fingerprint);

    if !is_registered {
        let is_tty = std::io::stdin().is_terminal();

        if is_tty {
            println!("SSH key not registered with Railway.");
            println!("Key: {} ({})", local_key.path.display(), local_key.fingerprint);
            println!();

            let should_register = prompt_confirm_with_default(
                "Register this SSH key with Railway?",
                true,
            )?;

            if !should_register {
                bail!("SSH key registration required for native SSH access.\n\
                       You can also register your key at: https://railway.com/account/ssh-keys");
            }
        } else {
            // Non-TTY: auto-register the key
            eprintln!("Registering SSH key with Railway...");
        }

        let key_name = local_key
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("ssh-key")
            .to_string();

        register_ssh_key(client, configs, &key_name, &local_key.public_key).await?;

        if is_tty {
            println!("SSH key registered successfully!");
        } else {
            eprintln!("SSH key registered successfully!");
        }
    }

    Ok(())
}

/// Check if native SSH is available (local SSH key exists)
pub fn native_ssh_available() -> bool {
    find_local_ssh_keys().map(|keys| !keys.is_empty()).unwrap_or(false)
}

/// Run SSH command with the given service instance ID
/// Note: This only works for interactive shells. Command execution requires relay mode
/// because Railway's SSH proxy doesn't forward exec commands through the QUIC tunnel.
pub fn run_native_ssh(
    service_instance_id: &str,
) -> Result<i32> {
    let target = format!("{}@{}", service_instance_id, SSH_HOST);

    let mut ssh_cmd = Command::new("ssh");
    ssh_cmd.arg(&target);

    // Interactive shell - inherit everything
    ssh_cmd.stdin(Stdio::inherit());
    ssh_cmd.stdout(Stdio::inherit());
    ssh_cmd.stderr(Stdio::inherit());

    let status = ssh_cmd.status().context("Failed to execute ssh command")?;

    Ok(status.code().unwrap_or(1))
}

/// Run SSH with tmux session
pub fn run_native_ssh_with_tmux(
    service_instance_id: &str,
    session_name: &str,
) -> Result<i32> {
    let target = format!("{}@{}", service_instance_id, SSH_HOST);
    let tmux_cmd = format!(
        "which tmux || (apt-get update && apt-get install -y tmux); exec tmux new-session -A -s {} \\; set -g mouse on",
        session_name
    );

    let mut ssh_cmd = Command::new("ssh");
    ssh_cmd.arg("-t"); // Force TTY allocation for tmux
    ssh_cmd.arg(&target);
    ssh_cmd.arg("--");
    ssh_cmd.arg(&tmux_cmd);

    ssh_cmd.stdin(Stdio::inherit());
    ssh_cmd.stdout(Stdio::inherit());
    ssh_cmd.stderr(Stdio::inherit());

    let status = ssh_cmd.status().context("Failed to execute ssh command")?;

    Ok(status.code().unwrap_or(1))
}
