//! The relay says an agent is running before it executes commands for it; in
//! between it answers ssh with a JSON status document. herdr reads that as an
//! unsupported platform and parks the machine in Attention, which it never
//! retries on its own. So a machine is only handed to herdr once a real
//! command has round-tripped.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::commands::code;
use crate::commands::ssh::config as ssh_config;
use crate::commands::ssh::native::run_native_ssh_captured;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;

const TIMEOUT: Duration = Duration::from_secs(150);
const PAUSE: Duration = Duration::from_secs(3);

pub async fn wait_until_ready(agent: &ca::Agent) -> Result<()> {
    let info = code::connect_info(&agent.environment_id, &agent.id).await?;
    // Herdr persists only the URI and invokes OpenSSH itself. Carry over the
    // identity used by this probe so its later connections use the same key.
    let config = ssh_config::expand_tilde(Path::new("~/.ssh/config"))?;
    let (host, _) = Configs::get_ssh_relay();
    ensure_identity(&config, host, &info.ssh_target, info.identity.as_deref()).await?;
    let nonce = format!("herdr-ready-{}", rand::random::<u32>());
    let started = Instant::now();
    loop {
        let target = info.ssh_target.clone();
        let identity = info.identity.clone();
        let mut opts = info.relay_opts.clone();
        opts.push("-o".into());
        opts.push("ConnectTimeout=10".into());
        let command = format!("echo {nonce}");
        let seen = tokio::task::spawn_blocking(move || {
            run_native_ssh_captured(&target, &command, identity.as_deref(), None, &opts)
        })
        .await?
        .map(|(_, stdout, _)| String::from_utf8_lossy(&stdout).contains(&nonce))
        .unwrap_or(false);
        if seen {
            return Ok(());
        }
        if started.elapsed() > TIMEOUT {
            bail!(
                "Agent {} is running but its ssh relay is not executing commands yet; retry in a minute.",
                agent.name
            );
        }
        tokio::time::sleep(PAUSE).await;
    }
}

async fn ensure_identity(
    config: &Path,
    host: &str,
    user: &str,
    identity: Option<&Path>,
) -> Result<()> {
    let marker = format!("herdr-{host}-{user}");
    let _lock = super::state::lock_file(&config.with_extension("herdr.lock")).await?;
    let Some(identity) = identity else {
        ssh_config::remove_marked_block(config, &marker)?;
        return Ok(());
    };
    // Match arguments are patterns. Relay hostnames and agent usernames must
    // remain literal rather than broadening the rule to other SSH connections.
    for value in [host, user] {
        if value.is_empty()
            || value
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '*' | '?' | '!' | ',' | '"' | '\\'))
        {
            bail!("Cannot write an SSH identity rule for {value:?}");
        }
    }
    let identity =
        ssh_config::quote_ssh_config_value(&identity.to_string_lossy().replace('%', "%%"));
    let block = format!(
        "# BEGIN railway:{marker}\n\
         Match host {host} user {user}\n\
             IdentityFile {identity}\n\
             IdentitiesOnly yes\n\
         Host *\n\
         # END railway:{marker}\n"
    );
    ssh_config::upsert_marked_block(config, &marker, &block)
        .with_context(|| format!("Configuring Herdr's SSH identity in {}", config.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn raw_herdr_uri_uses_the_selected_key_only_for_that_agent() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let key = dir.path().join("custom key");
        std::fs::write(&config, "Host workbox\n    IdentityFile /work/key\n").unwrap();
        ensure_identity(&config, "relay.example", "agent:env:a1", Some(&key))
            .await
            .unwrap();
        // An update replaces the rule rather than accumulating identity keys.
        let selected = dir.path().join("selected key");
        ensure_identity(&config, "relay.example", "agent:env:a1", Some(&selected))
            .await
            .unwrap();
        let resolved = |target| {
            let out = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(&config)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        let output = resolved("ssh://agent%3Aenv%3Aa1@relay.example:2222");
        assert!(output.contains("user agent:env:a1\n"), "{output}");
        assert!(
            output.contains(&format!("identityfile {}\n", selected.display())),
            "{output}"
        );
        assert!(output.contains("identitiesonly yes\n"), "{output}");
        assert!(!output.contains(&key.to_string_lossy().to_string()));
        for target in [
            "ssh://agent%3Aenv%3Aa2@relay.example",
            "ssh://agent%3Aenv%3Aa1@other.example",
            "workbox",
        ] {
            assert!(!resolved(target).contains(&selected.to_string_lossy().to_string()));
        }
        assert!(resolved("workbox").contains("identityfile /work/key\n"));
        ensure_identity(&config, "relay.example", "agent:env:a1", None)
            .await
            .unwrap();
        assert!(
            !resolved("ssh://agent%3Aenv%3Aa1@relay.example")
                .contains(&selected.to_string_lossy().to_string())
        );
    }
}
