use anyhow::{Context, Result, bail};
use reqwest::Client;
use std::collections::HashSet;
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
}

impl LocalSshKey {
    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| ssh_key_comment(&self.public_key).map(ToString::to_string))
            .unwrap_or_else(|| "ssh-agent".to_string())
    }

    pub fn display_source(&self) -> String {
        self.path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "ssh-agent".to_string())
    }

    pub fn default_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|name| name.to_str())
            .map(ToString::to_string)
            .or_else(|| ssh_key_comment(&self.public_key).map(sanitize_key_name))
            .unwrap_or_else(|| "ssh-agent-key".to_string())
    }

    pub fn source_label(&self) -> &'static str {
        if self.path.is_some() {
            "local (~/.ssh/)"
        } else {
            "ssh-agent"
        }
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

/// Find SSH keys available to local ssh, from ~/.ssh/*.pub and ssh-agent.
pub fn find_local_ssh_keys() -> Result<Vec<LocalSshKey>> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    let ssh_dir = home.join(".ssh");

    let mut keys = Vec::new();

    // Scan for all .pub files in ~/.ssh/
    if ssh_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&ssh_dir)
    {
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

    let mut seen = keys
        .iter()
        .map(|key| key.fingerprint.clone())
        .collect::<HashSet<_>>();
    for key in find_agent_ssh_keys() {
        if seen.insert(key.fingerprint.clone()) {
            keys.push(key);
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

fn find_agent_ssh_keys() -> Vec<LocalSshKey> {
    if std::env::var_os("SSH_AUTH_SOCK").is_none() {
        return Vec::new();
    }

    let Ok(output) = Command::new("ssh-add").arg("-L").output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    agent_keys_from_output(&stdout, compute_fingerprint_from_pubkey)
}

fn agent_keys_from_output<F>(output: &str, mut fingerprint: F) -> Vec<LocalSshKey>
where
    F: FnMut(&str) -> Result<String>,
{
    output
        .lines()
        .filter_map(|line| {
            let public_key = line.trim();
            let key_type = supported_key_type(public_key)?;
            let fingerprint = fingerprint(public_key).ok()?;
            Some(LocalSshKey {
                path: None,
                public_key: public_key.to_string(),
                fingerprint,
                key_type,
            })
        })
        .collect()
}

fn supported_key_type(public_key: &str) -> Option<String> {
    let key_type = public_key.split_whitespace().next()?;
    SUPPORTED_KEY_TYPES
        .iter()
        .any(|supported| key_type.starts_with(supported))
        .then(|| key_type.to_string())
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
        path: Some(path.to_path_buf()),
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

fn ssh_key_comment(public_key: &str) -> Option<&str> {
    public_key.split_whitespace().nth(2)
}

fn sanitize_key_name(value: &str) -> String {
    let name = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();

    if name.is_empty() {
        "ssh-agent-key".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA laptop";

    #[test]
    fn agent_keys_parse_supported_public_keys() {
        let output = format!("{ED25519_KEY}\nssh-unsupported AAAAC3Nza unsupported\n\nnot-a-key\n");

        let keys = agent_keys_from_output(&output, |key| Ok(format!("fp-{}", key.len())));

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].path, None);
        assert_eq!(keys[0].key_type, "ssh-ed25519");
        assert_eq!(keys[0].public_key, ED25519_KEY);
        assert!(keys[0].fingerprint.starts_with("fp-"));
    }

    #[test]
    fn agent_key_display_name_prefers_comment() {
        let key = LocalSshKey {
            path: None,
            public_key: ED25519_KEY.to_string(),
            fingerprint: "SHA256:test".to_string(),
            key_type: "ssh-ed25519".to_string(),
        };

        assert_eq!(key.display_name(), "laptop");
        assert_eq!(key.default_name(), "laptop");
        assert_eq!(key.display_source(), "ssh-agent");
        assert_eq!(key.source_label(), "ssh-agent");
    }
}
