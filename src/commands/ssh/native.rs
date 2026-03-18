use anyhow::{Context, Result, bail};
use is_terminal::IsTerminal;
use reqwest::Client;
use std::process::{Command, Stdio};

use crate::client::post_graphql;
use crate::config::Configs;
use crate::controllers::ssh_keys::{find_local_ssh_keys, register_ssh_key};
use crate::gql::queries::{ServiceInstance, service_instance};
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
        // Already registered - just use it
        eprintln!("Using SSH key: {}", key.path.display());
        return Ok(());
    }

    // No local key is registered - need to register one
    // Prefer ed25519, then ecdsa, then rsa
    let key_to_register = local_keys
        .iter()
        .find(|k| k.key_type.contains("ed25519"))
        .or_else(|| local_keys.iter().find(|k| k.key_type.contains("ecdsa")))
        .or_else(|| local_keys.first())
        .unwrap();

    let is_tty = std::io::stdin().is_terminal();

    if is_tty {
        println!("SSH key not registered with Railway.");
        println!(
            "Key: {} ({})",
            key_to_register.path.display(),
            key_to_register.fingerprint
        );
        println!();

        let should_register =
            prompt_confirm_with_default("Register this SSH key with Railway?", true)?;

        if !should_register {
            bail!(
                "SSH key registration required for native SSH access.\n\
                   You can also register your key at: https://railway.com/account/ssh-keys"
            );
        }
    } else {
        bail!(
            "SSH key registration required for native SSH access.\n\n\
            Key found but not registered: {} ({})\n\n\
            Register it with:\n  railway ssh keys add\n\n\
            Or import from GitHub:\n  railway ssh keys github",
            key_to_register.path.display(),
            key_to_register.fingerprint
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

/// Check if native SSH is available (local SSH key exists)
pub fn native_ssh_available() -> bool {
    find_local_ssh_keys()
        .map(|keys| !keys.is_empty())
        .unwrap_or(false)
}

/// Run SSH command with the given service instance ID
/// Optionally executes a command instead of starting an interactive shell
pub fn run_native_ssh(service_instance_id: &str, command: Option<&[String]>) -> Result<i32> {
    let target = format!("{}@{}", service_instance_id, SSH_HOST);

    let mut ssh_cmd = Command::new("ssh");
    ssh_cmd.arg(&target);

    if let Some(cmd_args) = command {
        // Pass command as SSH args (exec channel)
        for arg in cmd_args {
            ssh_cmd.arg(arg);
        }
        ssh_cmd.stdin(Stdio::inherit());
        ssh_cmd.stdout(Stdio::inherit());
        ssh_cmd.stderr(Stdio::inherit());

        let status = ssh_cmd.status().context("Failed to execute ssh command")?;
        Ok(status.code().unwrap_or(1))
    } else {
        // Interactive shell - inherit everything
        ssh_cmd.stdin(Stdio::inherit());
        ssh_cmd.stdout(Stdio::inherit());
        ssh_cmd.stderr(Stdio::inherit());

        let status = ssh_cmd.status().context("Failed to execute ssh command")?;
        Ok(status.code().unwrap_or(1))
    }
}
