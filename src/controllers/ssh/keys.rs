use crate::client::post_graphql;
use crate::config::Configs;
use crate::gql::mutations::{
    SshPublicKeyCreate, SshPublicKeyDelete, ValidateTwoFactor, ssh_public_key_create,
    ssh_public_key_delete, validate_two_factor,
};
use crate::gql::queries::{GitHubSshKeys, SshPublicKeys, git_hub_ssh_keys, ssh_public_keys};
use anyhow::{Context, Result, bail};
use reqwest::Client;
use russh::keys::HashAlg::Sha256;
use russh::keys::agent::client::AgentClient;
use russh::keys::{PublicKey, parse_public_key_base64};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum SshKeySource {
    Agent,
    File(PathBuf),
}

/// Local SSH key info
#[derive(Debug, Clone)]
pub struct LocalSshKey {
    pub source: SshKeySource,
    pub public_key: PublicKey,

    // metadata - mostly rederived?
    pub fingerprint: String,
    pub key_type: String,
    pub key_comment: Option<String>,
}

impl LocalSshKey {
    pub fn key_name(&self) -> Cow<'_, str> {
        if let Some(comment) = &self.key_comment {
            comment.into()
        } else if let SshKeySource::File(ref path) = self.source {
            path.file_stem()
                .map(|stem| stem.to_string_lossy())
                .unwrap_or_else(|| (&self.fingerprint).into())
        } else {
            (&self.fingerprint).into()
        }
    }

    pub fn key_source(&self) -> Cow<'_, str> {
        match self.source {
            SshKeySource::Agent => "SSH Agent".into(),
            SshKeySource::File(ref path) => path.to_string_lossy(),
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

pub async fn find_local_ssh_keys() -> Result<Vec<LocalSshKey>> {
    let mut seen = std::collections::HashMap::new();

    for key in fetch_keys_from_agent().await.unwrap_or_default() {
        seen.entry(key.fingerprint.clone()).or_insert(key);
    }

    for key in find_ssh_key_files()? {
        seen.entry(key.fingerprint.clone()).or_insert(key);
    }

    let mut keys = seen.into_values().collect::<Vec<_>>();
    keys.sort_by_key(|k| {
        let source_priority = match k.source {
            SshKeySource::Agent => 0,
            SshKeySource::File(_) => 1,
        };
        let type_priority = SUPPORTED_KEY_TYPES
            .iter()
            .position(|t| k.key_type.starts_with(t))
            .unwrap_or(usize::MAX);

        (source_priority, type_priority)
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

#[cfg(unix)]
pub async fn get_ssh_agent() -> Result<AgentClient<tokio::net::UnixStream>, russh::keys::Error> {
    AgentClient::connect_env().await
}

#[cfg(windows)]
pub async fn get_ssh_agent() -> Result<AgentClient<pageant::PageantStream>, russh::keys::Error> {
    AgentClient::connect_pageant().await
}

pub async fn fetch_keys_from_agent() -> Result<Vec<LocalSshKey>> {
    let mut agent: AgentClient<_> = get_ssh_agent().await?;
    let mut keys = Vec::new();

    agent
        .request_identities()
        .await?
        .into_iter()
        .for_each(|identity| {
            let public_key = identity.public_key();
            let key_type = public_key.algorithm().to_string();
            let key_comment = Some(identity.comment().to_string()).filter(|s| !s.is_empty());

            if SUPPORTED_KEY_TYPES.iter().any(|t| key_type.starts_with(t)) {
                keys.push(LocalSshKey {
                    source: SshKeySource::Agent,
                    public_key: (*public_key).clone(),
                    fingerprint: public_key.fingerprint(Sha256).to_string(),
                    key_type,
                    key_comment,
                });
            }
        });

    Ok(keys)
}

/// Read and parse an SSH public key file
/// FIXME: Not fully replaced with a russh call due to a lack of key comments.
///        See https://github.com/Eugeny/russh/issues/713.
fn read_ssh_key(path: &Path) -> Result<LocalSshKey> {
    let content = std::fs::read_to_string(path)?;
    let parts: Vec<&str> = content.split_whitespace().collect();

    if parts.len() < 2 {
        bail!("Invalid SSH key format");
    }

    let russh_key = parse_public_key_base64(parts[1])?;
    let key_comment = parts
        .get(2..)
        .map(|p| p.join(" "))
        .filter(|s| !s.is_empty());

    let fingerprint = russh_key.fingerprint(Sha256).to_string();

    Ok(LocalSshKey {
        source: SshKeySource::File(path.to_path_buf()),
        public_key: russh_key.clone(),
        fingerprint,
        key_type: russh_key.algorithm().to_string(),
        key_comment,
    })
}

/// Compute SHA256 fingerprint from a public key string
pub fn compute_fingerprint_from_pubkey(pubkey: &str) -> Result<String> {
    let key = parse_public_key_base64(pubkey)?;
    Ok(key.fingerprint(Sha256).to_string())
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
