use std::{io::IsTerminal, path::Path, sync::Arc};

use anyhow::{Context, Result, bail};
use russh::keys::{Algorithm, HashAlg, PrivateKeyWithHashAlg};

use crate::controllers::ssh_keys::{SshKeySource, find_local_ssh_keys, get_ssh_agent};
use crate::telemetry;

pub(super) async fn authenticate<H>(
    session: &mut russh::client::Handle<H>,
    username: &str,
) -> Result<()>
where
    H: russh::client::Handler,
{
    let candidates = find_local_ssh_keys().await?;
    if candidates.is_empty() {
        bail!("No SSH keys found.")
    }

    let mut load_errors = Vec::new();
    let mut attempted_auth = false;
    let mut agent = get_ssh_agent().await.ok();

    for candidate in &candidates {
        let success = match &candidate.source {
            SshKeySource::Agent => {
                let agent = match agent.as_mut() {
                    Some(a) => a,
                    None => continue,
                };
                attempted_auth = true;
                let hash_alg = rsa_hash_alg(session, candidate.public_key.algorithm()).await?;
                session
                    .authenticate_publickey_with(
                        username,
                        candidate.public_key.clone(),
                        hash_alg,
                        agent,
                    )
                    .await
                    .context("Failed to authenticate via SSH agent")?
                    .success()
            }
            SshKeySource::File(pubkey_path) => {
                let privkey_path = pubkey_path.with_extension("");
                let private_key = match load_secret_key(&privkey_path)? {
                    Ok(key) => key,
                    Err(err) => {
                        load_errors.push(format!(
                            "Failed to load private key from {}: {}",
                            privkey_path.display(),
                            err
                        ));
                        continue;
                    }
                };
                attempted_auth = true;

                let hash_alg = rsa_hash_alg(session, private_key.algorithm()).await?;
                session
                    .authenticate_publickey(
                        username,
                        PrivateKeyWithHashAlg::new(Arc::new(private_key), hash_alg),
                    )
                    .await
                    .context("Failed to authenticate via SSH key")?
                    .success()
            }
        };

        if success {
            return Ok(());
        }
    }

    if !attempted_auth {
        bail!(
            "No loadable SSH keys found.\n\nFor agents/non-interactive runs, use an unencrypted SSH key registered with Railway via `railway ssh keys add`, or run from an interactive terminal where a passphrase can be entered.\n\nSkipped keys:\n{}",
            load_errors.join("\n")
        );
    }

    bail!(
        "SSH authentication failed. Ensure one of your keys is registered with Railway using `railway ssh keys add`."
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
