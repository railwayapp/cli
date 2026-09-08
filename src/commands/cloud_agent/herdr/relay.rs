//! The relay says an agent is running before it executes commands for it; in
//! between it answers ssh with a JSON status document. herdr reads that as an
//! unsupported platform and parks the machine in Attention, which it never
//! retries on its own. So a machine is only handed to herdr once a real
//! command has round-tripped.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::commands::code;
use crate::commands::ssh::native::run_native_ssh_captured;
use crate::controllers::cloud_agent as ca;

const TIMEOUT: Duration = Duration::from_secs(150);
const PAUSE: Duration = Duration::from_secs(3);

pub async fn wait_until_ready(agent: &ca::Agent) -> Result<()> {
    let info = code::connect_info(&agent.environment_id, &agent.id).await?;
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
