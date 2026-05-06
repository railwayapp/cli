mod mongodb;
mod mysql;
mod postgres;
mod redis;
pub mod types;

use std::process::Stdio;

use anyhow::{Result, bail};
use tokio::io::AsyncWriteExt;

use crate::controllers::database::DatabaseType;
use crate::controllers::ssh_keys::find_local_ssh_keys;

pub use types::DatabaseStats;

const SSH_HOST: &str = "ssh.railway.com";

/// Preflight check for SSH-based DB stats collection. Runs locally only --
/// it catches the common "no SSH key" case before we spawn the SSH process,
/// so we can surface actionable guidance instead of a cryptic failure.
pub fn preflight_db_stats_ssh() -> Result<(), String> {
    match find_local_ssh_keys() {
        Ok(keys) if keys.is_empty() => Err(
            "no local SSH key found in ~/.ssh\n  \
             generate one with `ssh-keygen -t ed25519`, then register it with `railway ssh keys add`"
                .to_string(),
        ),
        Ok(_) => Ok(()),
        Err(e) => Err(format!(
            "unable to read ~/.ssh: {e}\n  \
             ensure the directory is readable and contains a registered SSH key"
        )),
    }
}

/// Translate a raw db-stats fetch error into a user-facing message with an
/// actionable next step. SSH and per-database CLI failures surface very
/// differently, so we try to classify the common modes and fall back to the
/// raw error otherwise.
pub fn diagnose_db_stats_failure(err: &anyhow::Error, db_type: &DatabaseType) -> String {
    let raw = format!("{err:#}");
    let lower = raw.to_ascii_lowercase();

    let hint = if lower.contains("permission denied (publickey)")
        || lower.contains("permission denied, please try again")
        || (lower.contains("permission denied") && lower.contains("publickey"))
    {
        Some(
            "your SSH key isn't registered with Railway -- run `railway ssh keys add` \
             (or import from GitHub with `railway ssh keys github`)",
        )
    } else if lower.contains("no such file or directory") && lower.contains("ssh")
        || lower.contains("program not found")
        || lower.contains("command not found")
    {
        Some(
            "the `ssh` binary was not found on PATH -- install OpenSSH and retry \
             (macOS: preinstalled; Linux: `apt install openssh-client` / equivalent)",
        )
    } else if lower.contains("host key verification failed") {
        Some(
            "SSH host key verification failed -- remove the stale entry with \
             `ssh-keygen -R ssh.railway.com` and retry",
        )
    } else if lower.contains("could not resolve hostname")
        || lower.contains("temporary failure in name resolution")
    {
        Some("could not resolve ssh.railway.com -- check your network connection")
    } else if lower.contains("connection refused")
        || lower.contains("connection timed out")
        || lower.contains("network is unreachable")
    {
        Some("could not connect to ssh.railway.com -- check your network and firewall rules")
    } else if lower.contains("not found") && cli_tool_missing(db_type, &lower) {
        Some(match db_type {
            DatabaseType::PostgreSQL => {
                "this image does not ship `psql`; database stats need the official Railway \
                 Postgres image"
            }
            DatabaseType::Redis => {
                "this image does not ship `redis-cli`; database stats need the official Railway \
                 Redis image"
            }
            DatabaseType::MySQL => {
                "this image does not ship `mysql`; database stats need the official Railway \
                 MySQL image"
            }
            DatabaseType::MongoDB => {
                "this image does not ship `mongosh`; database stats need the official Railway \
                 MongoDB image"
            }
        })
    } else {
        None
    };

    match hint {
        Some(h) => format!("{raw}\n  {h}"),
        None => raw,
    }
}

fn cli_tool_missing(db_type: &DatabaseType, lower_err: &str) -> bool {
    let tool = match db_type {
        DatabaseType::PostgreSQL => "psql",
        DatabaseType::Redis => "redis-cli",
        DatabaseType::MySQL => "mysql",
        DatabaseType::MongoDB => "mongosh",
    };
    lower_err.contains(tool)
}

/// Execute a shell command inside a service container via native SSH
/// (`ssh <instanceId>@ssh.railway.com`) and capture stdout.
async fn exec_command_in_container(service_instance_id: &str, command: &str) -> Result<String> {
    let target = format!("{service_instance_id}@{SSH_HOST}");

    let mut child = tokio::process::Command::new("ssh")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg(&target)
        .arg("sh")
        .arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(command.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    } else {
        bail!("Failed to open stdin for SSH command");
    }

    let output = child.wait_with_output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "SSH command failed (exit {}): {}",
            output.status,
            stderr.trim()
        );
    }

    Ok(String::from_utf8(output.stdout)?)
}

/// Fetch database-specific internal metrics by SSHing into the container
/// and running database CLI commands.
pub async fn fetch_db_stats(
    service_instance_id: &str,
    db_type: &DatabaseType,
) -> Result<DatabaseStats> {
    match db_type {
        DatabaseType::PostgreSQL => {
            let stats = postgres::fetch_postgres_stats(service_instance_id).await?;
            Ok(DatabaseStats::PostgreSQL(stats))
        }
        DatabaseType::Redis => {
            let stats = redis::fetch_redis_stats(service_instance_id).await?;
            Ok(DatabaseStats::Redis(stats))
        }
        DatabaseType::MySQL => {
            let stats = mysql::fetch_mysql_stats(service_instance_id).await?;
            Ok(DatabaseStats::MySQL(stats))
        }
        DatabaseType::MongoDB => {
            let stats = mongodb::fetch_mongo_stats(service_instance_id).await?;
            Ok(DatabaseStats::MongoDB(stats))
        }
    }
}

/// Split raw command output into sections by delimiter markers.
/// Returns a map of section_name -> section_content.
fn split_sections(output: &str) -> std::collections::HashMap<&str, &str> {
    let mut sections = std::collections::HashMap::new();
    let mut current_name: Option<&str> = None;
    let mut current_start = 0;

    // Walk the string by finding each line's byte range directly
    let mut pos = 0;
    for line in output.lines() {
        // Find where this line actually starts in the original string
        // (lines() may skip \r\n or \n, so advance pos past any line ending)
        let line_start = pos;
        pos += line.len();
        // Skip the line ending (\r\n or \n)
        if output.as_bytes().get(pos) == Some(&b'\r') {
            pos += 1;
        }
        if output.as_bytes().get(pos) == Some(&b'\n') {
            pos += 1;
        }

        if let Some(name) = line.strip_prefix("===").and_then(|s| s.strip_suffix("===")) {
            // Save previous section
            if let Some(prev) = current_name {
                let content = &output[current_start..line_start];
                sections.insert(prev, content.trim());
            }
            current_name = Some(name);
            current_start = pos;
        }
    }

    // Save last section
    if let Some(name) = current_name {
        let content = &output[current_start..];
        sections.insert(name, content.trim());
    }

    sections
}

/// Parse a simple integer from a string, returning 0 on failure.
fn parse_i64(s: &str) -> i64 {
    s.trim().parse().unwrap_or(0)
}

/// Parse a simple float from a string, returning 0.0 on failure.
fn parse_f64(s: &str) -> f64 {
    s.trim().parse().unwrap_or(0.0)
}
