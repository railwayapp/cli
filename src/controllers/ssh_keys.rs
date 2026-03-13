use anyhow::{Context, Result, bail};
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::client::post_graphql;
use crate::config::Configs;
use crate::gql::mutations::{
    SshPublicKeyCreate, SshPublicKeyDelete, ssh_public_key_create, ssh_public_key_delete,
};
use crate::gql::queries::{GitHubSshKeys, SshPublicKeys, git_hub_ssh_keys, ssh_public_keys};

/// Local SSH key info
#[derive(Debug, Clone)]
pub struct LocalSshKey {
    pub path: PathBuf,
    pub public_key: String,
    pub fingerprint: String,
    pub key_type: String,
}

/// Supported SSH key types (in order of preference)
const SUPPORTED_KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "ssh-rsa",
    "ssh-dss",
];

/// Find local SSH keys by scanning ~/.ssh/ for .pub files
pub fn find_local_ssh_keys() -> Result<Vec<LocalSshKey>> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    let ssh_dir = home.join(".ssh");

    if !ssh_dir.exists() {
        return Ok(vec![]);
    }

    let mut keys = Vec::new();

    // Scan for all .pub files in ~/.ssh/
    if let Ok(entries) = std::fs::read_dir(&ssh_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "pub") {
                if let Ok(key) = read_ssh_key(&path) {
                    // Only include supported key types
                    if SUPPORTED_KEY_TYPES
                        .iter()
                        .any(|t| key.key_type.starts_with(t))
                    {
                        keys.push(key);
                    }
                }
            }
        }
    }

    // Sort by key type preference (ed25519 first, then ecdsa, then rsa, then dss)
    keys.sort_by_key(|k| {
        SUPPORTED_KEY_TYPES
            .iter()
            .position(|t| k.key_type.starts_with(t))
            .unwrap_or(usize::MAX)
    });

    Ok(keys)
}

/// Read and parse an SSH public key file
fn read_ssh_key(path: &Path) -> Result<LocalSshKey> {
    let content = std::fs::read_to_string(path)?;
    let parts: Vec<&str> = content.split_whitespace().collect();

    if parts.len() < 2 {
        bail!("Invalid SSH key format");
    }

    let key_type = parts[0].to_string();
    let public_key = content.trim().to_string();

    // Compute fingerprint using ssh-keygen
    let fingerprint = compute_fingerprint(path)?;

    Ok(LocalSshKey {
        path: path.to_path_buf(),
        public_key,
        fingerprint,
        key_type,
    })
}

/// Compute SHA256 fingerprint of an SSH key file
pub fn compute_fingerprint(key_path: &Path) -> Result<String> {
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

/// Compute SHA256 fingerprint from a public key string
pub fn compute_fingerprint_from_pubkey(pubkey: &str) -> Result<String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("ssh-keygen")
        .args(["-lf", "-", "-E", "sha256"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to run ssh-keygen")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(pubkey.as_bytes())?;
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        bail!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
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

/// Delete an SSH public key from Railway
pub async fn delete_ssh_key(
    client: &Client,
    configs: &Configs,
    id: &str,
    two_factor_code: Option<String>,
) -> Result<bool> {
    let vars = ssh_public_key_delete::Variables {
        id: id.to_string(),
        code: two_factor_code,
    };

    let response =
        post_graphql::<SshPublicKeyDelete, _>(client, configs.get_backboard(), vars).await?;

    Ok(response.ssh_public_key_delete)
}

/// Get SSH public keys from the user's GitHub account
pub async fn get_github_ssh_keys(
    client: &Client,
    configs: &Configs,
) -> Result<Vec<git_hub_ssh_keys::GitHubSshKeysGitHubSshKeys>> {
    let vars = git_hub_ssh_keys::Variables {};
    let response = post_graphql::<GitHubSshKeys, _>(client, configs.get_backboard(), vars).await?;

    Ok(response.git_hub_ssh_keys)
}
