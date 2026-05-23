use anyhow::{Context, Result, bail};
use reqwest::Client;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::client::post_graphql;
use crate::config::Configs;
use crate::gql::mutations::{
    SshPublicKeyCreate, SshPublicKeyDelete, ValidateTwoFactor, ssh_public_key_create,
    ssh_public_key_delete, validate_two_factor,
};
use crate::gql::queries::{GitHubSshKeys, SshPublicKeys, git_hub_ssh_keys, ssh_public_keys};

/// Local SSH key info
#[derive(Debug, Clone)]
pub struct LocalSshKey {
    pub path: Option<PathBuf>,
    pub public_key: String,
    pub fingerprint: String,
    pub key_type: String,
    pub key_comment: Option<String>,
}

impl LocalSshKey {
    pub fn key_name(&self) -> Cow<'_, str> {
        match (self.key_comment.as_ref(), self.path.as_ref()) {
            (Some(comment), _) if !comment.is_empty() => comment.into(),
            (Some(_), None) => "SSH Agent Key".into(),
            (_, Some(path)) => path.file_stem().unwrap().to_string_lossy(),
            _ => (&self.fingerprint).into(),
        }
    }

    pub fn key_source(&self) -> Cow<'_, str> {
        self.path
            .as_ref()
            .map(|p| p.to_string_lossy())
            .unwrap_or_else(|| "SSH Agent".into())
    }
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

pub fn find_local_ssh_keys() -> Result<Vec<LocalSshKey>> {
    let mut seen = std::collections::HashMap::new();
    for key in fetch_keys_from_ssh_agent()? {
        seen.entry(key.fingerprint.clone()).or_insert(key);
    }

    for key in find_ssh_key_files()? {
        seen.entry(key.fingerprint.clone()).or_insert(key);
    }

    let mut keys = seen.into_values().collect::<Vec<_>>();
    keys.sort_by_key(|k| {
        SUPPORTED_KEY_TYPES
            .iter()
            .position(|t| k.key_type.starts_with(t))
            .unwrap_or(usize::MAX)
    });

    Ok(keys)
}

/// Find local SSH keys by scanning ~/.ssh/ for .pub files
pub fn find_ssh_key_files() -> Result<Vec<LocalSshKey>> {
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

    Ok(keys)
}

// Pull SSH keys from the agent directly.
pub fn fetch_keys_from_ssh_agent() -> Result<Vec<LocalSshKey>> {
    let output = Command::new("ssh-add")
        .arg("-L")
        .output()
        .context("Failed to run ssh-add -L")?;

    if !output.status.success() {
        bail!(
            "ssh-add -L failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8_lossy(&output.stdout)
        .split("\n")
        .filter(|s| !s.is_empty())
        .map(|s| {
            let parts: Vec<_> = s.split_whitespace().collect();
            let fingerprint = compute_fingerprint_from_pubkey(s)?;

            Ok(LocalSshKey {
                path: None,
                public_key: s.trim().to_string(),
                fingerprint,
                key_type: parts[0].to_string(),
                key_comment: parts[2..].join(" ").into(),
            })
        })
        .collect()
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
    let key_comment = parts[2..].join(" ").into();

    // Compute fingerprint using ssh-keygen
    let fingerprint = compute_fingerprint(path)?;

    Ok(LocalSshKey {
        path: Some(path.into()),
        public_key,
        fingerprint,
        key_type,
        key_comment,
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

/// Get registered SSH public keys. When `workspace_id` is `Some`, returns
/// the workspace's keys (requires workspace MEMBER+ access); otherwise
/// returns the authenticated user's personal keys.
pub async fn get_registered_ssh_keys(
    client: &Client,
    configs: &Configs,
    workspace_id: Option<String>,
) -> Result<Vec<ssh_public_keys::SshPublicKeysSshPublicKeysEdgesNode>> {
    let vars = ssh_public_keys::Variables { workspace_id };
    let response = post_graphql::<SshPublicKeys, _>(client, configs.get_backboard(), vars).await?;

    let keys: Vec<_> = response
        .ssh_public_keys
        .edges
        .into_iter()
        .map(|e| e.node)
        .collect();

    Ok(keys)
}

/// Register an SSH public key with Railway. Pass `workspace_id` to register
/// a workspace-owned key (requires workspace ADMIN access); otherwise the
/// key is registered to the authenticated user.
pub async fn register_ssh_key(
    client: &Client,
    configs: &Configs,
    name: &str,
    public_key: &str,
    workspace_id: Option<String>,
) -> Result<ssh_public_key_create::SshPublicKeyCreateSshPublicKeyCreate> {
    let vars = ssh_public_key_create::Variables {
        input: ssh_public_key_create::SshPublicKeyCreateInput {
            name: name.to_string(),
            public_key: public_key.to_string(),
            workspace_id,
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
    if let Some(token) = two_factor_code {
        let vars = validate_two_factor::Variables { token };
        post_graphql::<ValidateTwoFactor, _>(client, configs.get_backboard(), vars).await?;
    }

    let vars = ssh_public_key_delete::Variables { id: id.to_string() };
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
