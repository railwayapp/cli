use anyhow::{Context, Result, bail};
use reqwest::Client;
use std::path::PathBuf;
use std::process::Command;

use crate::client::post_graphql;
use crate::config::Configs;
use crate::gql::mutations::{ssh_public_key_create, SshPublicKeyCreate};
use crate::gql::queries::{ssh_public_keys, SshPublicKeys};

/// Local SSH key info
#[derive(Debug, Clone)]
pub struct LocalSshKey {
    pub path: PathBuf,
    pub public_key: String,
    pub fingerprint: String,
    pub key_type: String,
}

/// Find local SSH keys
pub fn find_local_ssh_keys() -> Result<Vec<LocalSshKey>> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    let ssh_dir = home.join(".ssh");

    if !ssh_dir.exists() {
        return Ok(vec![]);
    }

    let key_files = [
        "id_ed25519.pub",
        "id_ecdsa.pub",
        "id_rsa.pub",
        "id_dsa.pub",
    ];

    let mut keys = Vec::new();

    for key_file in key_files {
        let key_path = ssh_dir.join(key_file);
        if key_path.exists() {
            if let Ok(key) = read_ssh_key(&key_path) {
                keys.push(key);
            }
        }
    }

    Ok(keys)
}

/// Read and parse an SSH public key file
fn read_ssh_key(path: &PathBuf) -> Result<LocalSshKey> {
    let content = std::fs::read_to_string(path)?;
    let parts: Vec<&str> = content.trim().split_whitespace().collect();

    if parts.len() < 2 {
        bail!("Invalid SSH key format");
    }

    let key_type = parts[0].to_string();
    let public_key = content.trim().to_string();

    // Compute fingerprint using ssh-keygen
    let fingerprint = compute_fingerprint(path)?;

    Ok(LocalSshKey {
        path: path.clone(),
        public_key,
        fingerprint,
        key_type,
    })
}

/// Compute SHA256 fingerprint of an SSH key
pub fn compute_fingerprint(key_path: &PathBuf) -> Result<String> {
    let output = Command::new("ssh-keygen")
        .args(["-lf", key_path.to_str().unwrap(), "-E", "sha256"])
        .output()
        .context("Failed to run ssh-keygen")?;

    if !output.status.success() {
        bail!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    // Format: "256 SHA256:xxxxx comment (TYPE)"
    // We want "SHA256:xxxxx"
    let parts: Vec<&str> = output_str.split_whitespace().collect();
    if parts.len() >= 2 {
        Ok(parts[1].to_string())
    } else {
        bail!("Could not parse fingerprint from ssh-keygen output");
    }
}

/// Get all SSH public keys registered for the current user
pub async fn get_registered_ssh_keys(
    client: &Client,
    configs: &Configs,
) -> Result<Vec<ssh_public_keys::SshPublicKeysSshPublicKeysEdgesNode>> {
    let vars = ssh_public_keys::Variables {};
    let response = post_graphql::<SshPublicKeys, _>(client, configs.get_backboard(), vars).await?;

    let keys: Vec<_> = response
        .ssh_public_keys
        .edges
        .into_iter()
        .map(|e| e.node)
        .collect();

    Ok(keys)
}

/// Register an SSH public key with Railway
pub async fn register_ssh_key(
    client: &Client,
    configs: &Configs,
    name: &str,
    public_key: &str,
) -> Result<ssh_public_key_create::SshPublicKeyCreateSshPublicKeyCreate> {
    let vars = ssh_public_key_create::Variables {
        input: ssh_public_key_create::SshPublicKeyCreateInput {
            name: name.to_string(),
            public_key: public_key.to_string(),
        },
    };

    let response =
        post_graphql::<SshPublicKeyCreate, _>(client, configs.get_backboard(), vars).await?;

    Ok(response.ssh_public_key_create)
}

/// Check if any local SSH key is registered with Railway
/// Returns the local key that matches a registered one, if any
pub async fn find_registered_local_key(
    client: &Client,
    configs: &Configs,
) -> Result<Option<LocalSshKey>> {
    let local_keys = find_local_ssh_keys()?;
    if local_keys.is_empty() {
        return Ok(None);
    }

    let registered_keys = get_registered_ssh_keys(client, configs).await?;

    for local_key in &local_keys {
        for registered in &registered_keys {
            if registered.fingerprint == local_key.fingerprint {
                return Ok(Some(local_key.clone()));
            }
        }
    }

    Ok(None)
}

/// Ensure at least one local SSH key is registered with Railway
/// Returns the local key that is (or was just) registered
pub async fn ensure_ssh_key_registered(
    client: &Client,
    configs: &Configs,
) -> Result<LocalSshKey> {
    let local_keys = find_local_ssh_keys()?;

    if local_keys.is_empty() {
        bail!(
            "No SSH keys found in ~/.ssh/\n\n\
            Generate one with:\n  ssh-keygen -t ed25519\n\n\
            Then run this command again."
        );
    }

    // Check if any local key is already registered
    if let Some(registered_key) = find_registered_local_key(client, configs).await? {
        return Ok(registered_key);
    }

    // No local key is registered - return the best candidate for registration
    // Prefer ed25519, then ecdsa, then rsa
    let key_to_register = local_keys
        .iter()
        .find(|k| k.key_type.contains("ed25519"))
        .or_else(|| local_keys.iter().find(|k| k.key_type.contains("ecdsa")))
        .or_else(|| local_keys.first())
        .unwrap();

    Ok(key_to_register.clone())
}
