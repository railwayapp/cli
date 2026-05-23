use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use russh::keys::{Algorithm, HashAlg, PrivateKeyWithHashAlg};

use crate::{controllers::ssh_keys::find_ssh_key_files, telemetry};

pub(super) async fn authenticate<H>(
    session: &mut russh::client::Handle<H>,
    username: &str,
) -> Result<()>
where
    H: russh::client::Handler,
{
    let key_paths = discover_private_key_paths()?;

    if key_paths.is_empty() {
        bail!(
            "No SSH private keys were found in ~/.ssh. Generate one with `ssh-keygen -t ed25519`, then register it with `railway ssh keys add`."
        );
    }

    let mut load_errors = Vec::new();
    let mut attempted_auth = false;

    for path in key_paths {
        let key = match load_secret_key(&path)? {
            Ok(key) => key,
            Err(err) => {
                load_errors.push(format!("{}: {err}", path.display()));
                continue;
            }
        };

        attempted_auth = true;
        let hash_alg = rsa_hash_alg(session, key.algorithm()).await?;
        let auth_result = session
            .authenticate_publickey(
                username,
                PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
            )
            .await
            .with_context(|| format!("Failed to authenticate with SSH key {}", path.display()))?;

        if auth_result.success() {
            return Ok(());
        }
    }

    if attempted_auth {
        bail!(
            "SSH authentication failed. Ensure one of your ~/.ssh keys is registered with Railway using `railway ssh keys add`."
        );
    }

    bail!(
        "No loadable SSH private keys were found in ~/.ssh.\n\nFor agents/non-interactive runs, use an unencrypted SSH key registered with Railway via `railway ssh keys add`, or run this command from an interactive terminal where a passphrase can be entered.\n\nSkipped keys:\n{}",
        load_errors.join("\n")
    );
}

fn load_secret_key(path: &Path) -> Result<Result<russh::keys::PrivateKey, anyhow::Error>> {
    match russh::keys::load_secret_key(path, None) {
        Ok(key) => Ok(Ok(key)),
        Err(err) if telemetry::is_agent() || !std::io::stdin().is_terminal() => {
            Ok(Err(anyhow::anyhow!(
                "{err}. If this key is encrypted, agents cannot enter SSH key passphrases. Add an unencrypted key with `railway ssh keys add --key {}` or run from an interactive terminal.",
                path.display()
            )))
        }
        Err(_err) => {
            let passphrase =
                inquire::Password::new(&format!("Enter passphrase for SSH key {}", path.display()))
                    .without_confirmation()
                    .with_render_config(crate::commands::Configs::get_render_config())
                    .prompt()
                    .context("Failed to prompt for SSH key passphrase")?;

            Ok(russh::keys::load_secret_key(path, Some(&passphrase))
                .with_context(|| format!("Failed to load SSH key {}", path.display())))
        }
    }
}

fn discover_private_key_paths() -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    for public_key in find_ssh_key_files()? {
        if public_key.path.is_none() {
            continue;
        }

        if let Some(private_key_path) = private_key_path_for_public_key(&public_key.path.unwrap()) {
            if private_key_path.is_file() {
                paths.push(private_key_path);
            }
        }
    }

    Ok(paths)
}

fn private_key_path_for_public_key(public_key_path: &Path) -> Option<PathBuf> {
    let file_name = public_key_path.file_name()?.to_str()?;
    let private_key_file_name = file_name.strip_suffix(".pub")?;
    Some(public_key_path.with_file_name(private_key_file_name))
}

async fn rsa_hash_alg<H>(
    session: &russh::client::Handle<H>,
    algorithm: Algorithm,
) -> Result<Option<HashAlg>>
where
    H: russh::client::Handler,
{
    if !matches!(algorithm, Algorithm::Rsa { .. }) {
        return Ok(None);
    }

    Ok(session
        .best_supported_rsa_hash()
        .await
        .context("Failed to determine server-supported RSA signature algorithm")?
        .flatten())
}
