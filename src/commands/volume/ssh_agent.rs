use anyhow::{Context, Result, bail};
use russh::keys::{
    Algorithm, HashAlg,
    agent::{AgentIdentity, client::AgentClient, client::AgentStream},
};

pub(super) async fn authenticate<H>(
    session: &mut russh::client::Handle<H>,
    username: &str,
) -> Result<()>
where
    H: russh::client::Handler,
{
    let mut agent = connect_env().await?;
    let identities = request_identities(&mut agent).await?;
    authenticate_with_identities(session, username, &mut agent, identities).await
}

pub(super) async fn connect_env() -> Result<AgentClient<tokio::net::UnixStream>> {
    AgentClient::connect_env()
        .await
        .context("Failed to connect to ssh-agent via SSH_AUTH_SOCK")
}

pub(super) async fn request_identities<S>(agent: &mut AgentClient<S>) -> Result<Vec<AgentIdentity>>
where
    S: AgentStream + Unpin,
{
    let identities = agent
        .request_identities()
        .await
        .context("Failed to list identities from ssh-agent")?;

    if identities.is_empty() {
        bail!(
            "No SSH keys are loaded in ssh-agent. Add one with `ssh-add`, then retry this command."
        );
    }

    Ok(identities)
}

pub(super) async fn authenticate_with_identities<H, S>(
    session: &mut russh::client::Handle<H>,
    username: &str,
    agent: &mut AgentClient<S>,
    identities: Vec<AgentIdentity>,
) -> Result<()>
where
    H: russh::client::Handler,
    S: AgentStream + Send + Unpin + 'static,
{
    for identity in identities {
        let auth_result = match identity {
            AgentIdentity::PublicKey { key, .. } => {
                let hash_alg = rsa_hash_alg(session, key.algorithm()).await?;
                session
                    .authenticate_publickey_with(username, key, hash_alg, agent)
                    .await
                    .context("Failed to authenticate with ssh-agent public key")?
            }
            AgentIdentity::Certificate { certificate, .. } => {
                let hash_alg = rsa_hash_alg(session, certificate.public_key().algorithm()).await?;
                session
                    .authenticate_certificate_with(username, certificate, hash_alg, agent)
                    .await
                    .context("Failed to authenticate with ssh-agent certificate")?
            }
        };

        if auth_result.success() {
            return Ok(());
        }
    }

    bail!(
        "SSH authentication failed. Ensure a Railway-registered key is loaded in ssh-agent with `ssh-add`."
    ); // TODO: look if bail is the right option here, prob not
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
