use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use russh::keys::{Algorithm, HashAlg, PrivateKeyWithHashAlg};

use crate::controllers::ssh_keys::find_local_ssh_keys;

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
        let key = match russh::keys::load_secret_key(&path, None) {
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
        "No loadable SSH private keys were found in ~/.ssh. Encrypted keys are not supported for volume SFTP yet.\n\nSkipped keys:\n{}",
        load_errors.join("\n")
    );
}

fn discover_private_key_paths() -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    for public_key in find_local_ssh_keys()? {
        if let Some(private_key_path) = private_key_path_for_public_key(&public_key.path) {
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
