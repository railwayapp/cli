//! herdr's saved-machine probe and every background reconnect run plain `ssh`
//! with `StrictHostKeyChecking=yes`, which never prompts, so the relay's key
//! has to be in `~/.ssh/known_hosts` before `machine add`. The railway CLI keeps
//! its verified copy in `~/.railway/known_hosts_relay`; this copies it across.

use std::path::Path;

use anyhow::{Context, Result};

use crate::config::Configs;

/// herdr's background reconnects run `ssh` with `StrictHostKeyChecking=yes`
/// and no config of their own, so a user `Host` block that points the relay
/// at `UserKnownHostsFile /dev/null` parks every machine in Attention after
/// the first network blip. `ssh -G` shows what would actually be used.
pub fn ssh_config_warning() -> Option<String> {
    let (host, _) = Configs::get_ssh_relay();
    let out = std::process::Command::new("ssh")
        .args(["-G", host])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let files: Vec<&str> = text
        .lines()
        .find_map(|l| l.strip_prefix("userknownhostsfile "))
        .map(|v| v.split_whitespace().collect())
        .unwrap_or_default();
    if files.iter().all(|f| *f == "/dev/null") {
        return Some(format!(
            "Your ssh config sends {host}'s host keys to /dev/null (UserKnownHostsFile). herdr reconnects with strict checking and will park every Railway machine in Attention; remove {host} from that Host block."
        ));
    }
    None
}

pub fn ensure_relay_known_host() -> Result<Seeded> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to get home directory"))?;
    let (host, _) = Configs::get_ssh_relay();
    ensure_in(
        &home.join(".railway").join("known_hosts_relay"),
        &home.join(".ssh").join("known_hosts"),
        host,
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum Seeded {
    Present,
    Added,
    /// The CLI has not connected to the relay yet, so there is nothing to copy.
    NoSource,
}

fn ensure_in(source: &Path, known_hosts: &Path, host: &str) -> Result<Seeded> {
    let Ok(relay) = std::fs::read_to_string(source) else {
        return Ok(Seeded::NoSource);
    };
    let Some(line) = relay
        .lines()
        .find(|l| is_ed25519_for(l, host))
        .map(str::trim)
    else {
        return Ok(Seeded::NoSource);
    };
    let existing = std::fs::read_to_string(known_hosts).unwrap_or_default();
    if existing.lines().any(|l| is_ed25519_for(l, host)) {
        return Ok(Seeded::Present);
    }
    if let Some(parent) = known_hosts.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(line);
    out.push_str(" railway-ca-herdr");
    out.push('\n');
    std::fs::write(known_hosts, out)
        .with_context(|| format!("Writing {}", known_hosts.display()))?;
    Ok(Seeded::Added)
}

fn is_ed25519_for(line: &str, host: &str) -> bool {
    let mut parts = line.split_whitespace();
    let hosts = parts.next().unwrap_or_default();
    parts.next() == Some("ssh-ed25519") && hosts.split(',').any(|h| h == host)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELAY: &str = "ssh.railway.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJ8X3z81relaykey\n";

    #[test]
    fn adds_once_and_keeps_existing_lines() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("known_hosts_relay");
        let kh = dir.path().join("ssh").join("known_hosts");
        std::fs::write(&src, RELAY).unwrap();
        assert_eq!(
            ensure_in(&src, &kh, "ssh.railway.com").unwrap(),
            Seeded::Added
        );
        std::fs::write(
            &kh,
            format!(
                "{}other ssh-rsa AAAA\n",
                std::fs::read_to_string(&kh).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(
            ensure_in(&src, &kh, "ssh.railway.com").unwrap(),
            Seeded::Present
        );
        let text = std::fs::read_to_string(&kh).unwrap();
        assert_eq!(text.matches("ssh.railway.com").count(), 1, "{text}");
        assert!(text.contains("relaykey railway-ca-herdr\n"), "{text}");
        assert!(text.contains("other ssh-rsa"), "{text}");
    }

    #[test]
    fn rsa_only_entry_does_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("relay");
        let kh = dir.path().join("known_hosts");
        std::fs::write(&src, RELAY).unwrap();
        std::fs::write(&kh, "ssh.railway.com ssh-rsa AAAAB3\n").unwrap();
        assert_eq!(
            ensure_in(&src, &kh, "ssh.railway.com").unwrap(),
            Seeded::Added
        );
    }

    #[test]
    fn missing_source_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            ensure_in(
                &dir.path().join("nope"),
                &dir.path().join("kh"),
                "ssh.railway.com"
            )
            .unwrap(),
            Seeded::NoSource
        );
    }
}
