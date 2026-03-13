use std::fmt;

use anyhow::{Result, bail};
use clap::Parser;
use is_terminal::IsTerminal;

use crate::client::GQLClient;
use crate::config::Configs;
use crate::controllers::ssh_keys::{
    LocalSshKey, delete_ssh_key, find_local_ssh_keys, get_registered_ssh_keys, register_ssh_key,
};
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
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Some(Commands::List) | None => list_keys().await,
        Some(Commands::Add { key, name }) => add_key(key, name).await,
        Some(Commands::Remove {
            key,
            two_factor_code,
        }) => remove_key(key, two_factor_code).await,
    }
}

async fn list_keys() -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let registered_keys = get_registered_ssh_keys(&client, &configs).await?;
    let local_keys = find_local_ssh_keys()?;

    if registered_keys.is_empty() {
        println!("No SSH keys registered with Railway.");
        println!();
        println!("Add a key with: railway ssh keys add");
        println!("Or register at: https://railway.com/account/ssh-keys");
        return Ok(());
    }

    println!("Registered SSH Keys:");
    println!();

    for key in &registered_keys {
        let local_match = local_keys.iter().find(|l| l.fingerprint == key.fingerprint);
        let local_indicator = if local_match.is_some() {
            " (local)"
        } else {
            ""
        };

        println!("  {} {}{}", key.name, key.fingerprint, local_indicator);
    }

    println!();

    // Show local keys that aren't registered
    let unregistered: Vec<_> = local_keys
        .iter()
        .filter(|l| {
            !registered_keys
                .iter()
                .any(|r| r.fingerprint == l.fingerprint)
        })
        .collect();

    if !unregistered.is_empty() {
        println!("Local keys not registered:");
        for key in unregistered {
            println!(
                "  {} {}",
                key.path.file_name().unwrap_or_default().to_string_lossy(),
                key.fingerprint
            );
        }
        println!();
        println!("Add with: railway ssh keys add");
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
