use std::fmt;

use anyhow::{Result, bail};
use clap::Parser;
use is_terminal::IsTerminal;

use crate::client::GQLClient;
use crate::config::Configs;
use crate::controllers::ssh_keys::{
    LocalSshKey, compute_fingerprint_from_pubkey, delete_ssh_key, find_local_ssh_keys,
    get_github_ssh_keys, get_registered_ssh_keys, register_ssh_key,
};
use crate::gql::queries::git_hub_ssh_keys::GitHubSshKeysGitHubSshKeys;
use crate::gql::queries::ssh_public_keys::SshPublicKeysSshPublicKeysEdgesNode;
use crate::util::prompt::{prompt_options, prompt_text};

/// Wrapper for LocalSshKey to implement Display for prompts
struct LocalKeyOption(LocalSshKey);

impl fmt::Display for LocalKeyOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({})",
            self.0
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            self.0.fingerprint
        )
    }
}

/// Wrapper for registered SSH key to implement Display for prompts
struct RegisteredKeyOption(SshPublicKeysSshPublicKeysEdgesNode);

impl fmt::Display for RegisteredKeyOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.0.name, self.0.fingerprint)
    }
}

/// Wrapper for GitHub SSH key to implement Display for prompts
struct GitHubKeyOption(GitHubSshKeysGitHubSshKeys);

impl fmt::Display for GitHubKeyOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<&str> = self.0.key.split_whitespace().collect();
        let key_type = parts.first().unwrap_or(&"unknown");
        let fingerprint = compute_fingerprint_from_pubkey(&self.0.key).unwrap_or_default();
        if fingerprint.is_empty() {
            write!(f, "{} ({})", self.0.title, key_type)
        } else {
            write!(f, "{} ({}) {}", self.0.title, key_type, fingerprint)
        }
    }
}

/// Manage SSH keys registered with Railway
#[derive(Parser, Clone)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Parser, Clone)]
enum Commands {
    /// List all registered SSH keys
    #[clap(alias = "ls")]
    List,

    /// Add/register a local SSH key with Railway
    Add {
        /// Path to the public key file (defaults to auto-detect)
        #[clap(long, short)]
        key: Option<String>,

        /// Name for the key (defaults to filename)
        #[clap(long, short)]
        name: Option<String>,
    },

    /// Remove a registered SSH key
    #[clap(alias = "rm", alias = "delete")]
    Remove {
        /// Key ID or fingerprint to remove
        key: Option<String>,

        /// 2FA code (required if 2FA is enabled)
        #[clap(long = "2fa-code")]
        two_factor_code: Option<String>,
    },

    /// Import SSH keys from your GitHub account
    #[clap(alias = "import")]
    Github,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Some(Commands::List) | None => list_keys().await,
        Some(Commands::Add { key, name }) => add_key(key, name).await,
        Some(Commands::Remove {
            key,
            two_factor_code,
        }) => remove_key(key, two_factor_code).await,
        Some(Commands::Github) => import_github_keys().await,
    }
}

async fn list_keys() -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let registered_keys = get_registered_ssh_keys(&client, &configs).await?;
    let local_keys = find_local_ssh_keys()?;
    let github_keys = get_github_ssh_keys(&client, &configs)
        .await
        .unwrap_or_default();

    // Show registered Railway keys
    if !registered_keys.is_empty() {
        println!("Registered SSH Keys:");

        for key in &registered_keys {
            let local_match = local_keys.iter().find(|l| l.fingerprint == key.fingerprint);

            // Extract comment/hostname from public key
            let parts: Vec<&str> = key.public_key.split_whitespace().collect();
            let key_type = parts.first().unwrap_or(&"");
            let hostname = parts.get(2).unwrap_or(&"");

            println!("  {}", key.name);
            println!("    Fingerprint: {}", key.fingerprint);
            if !key_type.is_empty() {
                println!("    Type:        {}", key_type);
            }
            if !hostname.is_empty() {
                println!("    Hostname:    {}", hostname);
            }
            if local_match.is_some() {
                println!("    Source:      local (~/.ssh/)");
            }
            println!();
        }
    }

    // Show GitHub keys
    if !github_keys.is_empty() {
        println!("GitHub SSH Keys:");
        for key in &github_keys {
            let parts: Vec<&str> = key.key.split_whitespace().collect();
            let key_type = parts.first().unwrap_or(&"unknown");
            let hostname = parts.get(2).unwrap_or(&"");
            let fingerprint = compute_fingerprint_from_pubkey(&key.key).unwrap_or_default();

            // Check if already registered
            let is_registered = registered_keys.iter().any(|r| r.fingerprint == fingerprint);

            println!("  {}", key.title);
            if !fingerprint.is_empty() {
                println!("    Fingerprint: {}", fingerprint);
            }
            println!("    Type:        {}", key_type);
            if !hostname.is_empty() {
                println!("    Hostname:    {}", hostname);
            }
            if is_registered {
                println!("    Status:      registered");
            }
            println!();
        }

        let has_unregistered = github_keys.iter().any(|gh| {
            let fp = compute_fingerprint_from_pubkey(&gh.key).unwrap_or_default();
            !registered_keys.iter().any(|r| r.fingerprint == fp)
        });
        if has_unregistered {
            println!("Import with:\n    railway ssh keys github");
            println!();
        }
    }

    // Show local keys that aren't registered
    let unregistered: Vec<_> = local_keys
        .iter()
        .filter(|l| {
            !registered_keys
                .iter()
                .any(|r| r.fingerprint == l.fingerprint)
        })
        .collect();

    if registered_keys.is_empty() && github_keys.is_empty() {
        println!("No SSH keys registered with Railway.");
        println!();
        println!("Add a key with: railway ssh keys add");
        println!("Or register at: https://railway.com/account/ssh-keys");
        return Ok(());
    }

    if !unregistered.is_empty() {
        println!("Local Keys (not registered):");
        for key in unregistered {
            // Extract hostname from public key
            let parts: Vec<&str> = key.public_key.split_whitespace().collect();
            let hostname = parts.get(2).unwrap_or(&"");

            println!(
                "  {}",
                key.path.file_name().unwrap_or_default().to_string_lossy()
            );
            println!("    Fingerprint: {}", key.fingerprint);
            println!("    Type:        {}", key.key_type);
            if !hostname.is_empty() {
                println!("    Hostname:    {}", hostname);
            }
            println!("    Path:        {}", key.path.display());
            println!();
        }
        println!("Add with:\n    railway ssh keys add");
    }

    Ok(())
}

async fn add_key(key_path: Option<String>, name: Option<String>) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let local_keys = find_local_ssh_keys()?;
    if local_keys.is_empty() {
        bail!(
            "No SSH keys found in ~/.ssh/\n\n\
            Generate one with:\n  ssh-keygen -t ed25519\n\n\
            Then run this command again."
        );
    }

    let registered_keys = get_registered_ssh_keys(&client, &configs).await?;

    // Filter to unregistered keys
    let unregistered: Vec<_> = local_keys
        .iter()
        .filter(|l| {
            !registered_keys
                .iter()
                .any(|r| r.fingerprint == l.fingerprint)
        })
        .collect();

    if unregistered.is_empty() {
        println!("All local SSH keys are already registered with Railway.");
        return Ok(());
    }

    // Select key to add
    let key_to_add = if let Some(path) = key_path {
        // Find by path
        local_keys
            .iter()
            .find(|k| k.path.to_string_lossy().contains(&path))
            .ok_or_else(|| anyhow::anyhow!("Key not found: {}", path))?
            .clone()
    } else if unregistered.len() == 1 {
        unregistered[0].clone()
    } else if std::io::stdin().is_terminal() {
        let options: Vec<LocalKeyOption> = unregistered
            .into_iter()
            .map(|k| LocalKeyOption(k.clone()))
            .collect();
        let selected = prompt_options("Select a key to register", options)?;
        selected.0
    } else {
        // Non-interactive: use first (preferred) key
        unregistered[0].clone()
    };

    // Determine name
    let key_name = name.unwrap_or_else(|| {
        key_to_add
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("ssh-key")
            .to_string()
    });

    println!(
        "Registering key: {} ({})",
        key_to_add.path.display(),
        key_to_add.fingerprint
    );

    register_ssh_key(&client, &configs, &key_name, &key_to_add.public_key).await?;

    println!("SSH key '{}' registered successfully!", key_name);

    Ok(())
}

async fn remove_key(key: Option<String>, two_factor_code: Option<String>) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let registered_keys = get_registered_ssh_keys(&client, &configs).await?;

    if registered_keys.is_empty() {
        println!("No SSH keys registered with Railway.");
        return Ok(());
    }

    // Select key to remove
    let key_to_remove = if let Some(key_id) = key {
        // Find by ID or fingerprint
        registered_keys
            .into_iter()
            .find(|k| k.id == key_id || k.fingerprint == key_id || k.name == key_id)
            .ok_or_else(|| anyhow::anyhow!("Key not found: {}", key_id))?
    } else if std::io::stdin().is_terminal() {
        let options: Vec<RegisteredKeyOption> = registered_keys
            .into_iter()
            .map(RegisteredKeyOption)
            .collect();
        let selected = prompt_options("Select a key to remove", options)?;
        selected.0
    } else {
        bail!("Key ID or fingerprint required in non-interactive mode");
    };

    // Get 2FA code if needed and not provided
    let code = if two_factor_code.is_some() {
        two_factor_code
    } else {
        None
    };

    println!(
        "Removing key: {} ({})",
        key_to_remove.name, key_to_remove.fingerprint
    );

    match delete_ssh_key(&client, &configs, &key_to_remove.id, code).await {
        Ok(true) => {
            println!("SSH key '{}' removed successfully!", key_to_remove.name);
            Ok(())
        }
        Ok(false) => {
            bail!("Failed to remove SSH key");
        }
        Err(e) => {
            // Check if it's a 2FA error
            let err_str = e.to_string();
            if err_str.contains("2FA")
                || err_str.contains("two-factor")
                || err_str.contains("verification")
            {
                if std::io::stdin().is_terminal() {
                    let code = prompt_text("Enter 2FA code")?;
                    delete_ssh_key(&client, &configs, &key_to_remove.id, Some(code)).await?;
                    println!("SSH key '{}' removed successfully!", key_to_remove.name);
                    Ok(())
                } else {
                    bail!("2FA code required. Use --2fa-code option.");
                }
            } else {
                Err(e)
            }
        }
    }
}

async fn import_github_keys() -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    println!("Fetching SSH keys from GitHub...");

    let github_keys = get_github_ssh_keys(&client, &configs).await?;

    if github_keys.is_empty() {
        println!("No SSH keys found in your GitHub account.");
        println!();
        println!("Add SSH keys to GitHub at: https://github.com/settings/keys");
        return Ok(());
    }

    let registered_keys = get_registered_ssh_keys(&client, &configs).await?;

    // Filter to keys not already registered (compare by fingerprint)
    let unregistered: Vec<_> = github_keys
        .iter()
        .filter(|gh| {
            let gh_fingerprint = compute_fingerprint_from_pubkey(&gh.key).unwrap_or_default();
            !registered_keys
                .iter()
                .any(|r| r.fingerprint == gh_fingerprint)
        })
        .collect();

    if unregistered.is_empty() {
        println!("All GitHub SSH keys are already registered with Railway.");
        return Ok(());
    }

    println!(
        "Found {} GitHub key(s) not yet registered:",
        unregistered.len()
    );
    for key in &unregistered {
        let parts: Vec<&str> = key.key.split_whitespace().collect();
        let key_type = parts.first().unwrap_or(&"unknown");
        let fingerprint = compute_fingerprint_from_pubkey(&key.key).unwrap_or_default();
        println!("  - {} ({}) {}", key.title, key_type, fingerprint);
    }
    println!();

    // Select key to import
    let key_to_import = if unregistered.len() == 1 {
        unregistered[0].clone()
    } else if std::io::stdin().is_terminal() {
        let options: Vec<GitHubKeyOption> = unregistered
            .into_iter()
            .map(|k| GitHubKeyOption(k.clone()))
            .collect();
        let selected = prompt_options("Select a key to import", options)?;
        selected.0
    } else {
        // Non-interactive: import first key
        unregistered[0].clone()
    };

    println!("Importing key: {}", key_to_import.title);

    register_ssh_key(&client, &configs, &key_to_import.title, &key_to_import.key).await?;

    println!("SSH key '{}' imported successfully!", key_to_import.title);

    Ok(())
}
