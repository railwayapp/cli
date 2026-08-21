//! One SSH connection to the relay, many commands.
//!
//! A launch runs four commands against the same agent (readiness probe,
//! provision, skills sync, then the session). Shelling out to `ssh` once per
//! command pays the relay's routing cost every time: `session.Route` and
//! `session.WaitForTunnel` are emitted per *connection*, before the channel
//! loop (`tcp-proxy/handlers/ssh_listener/main.go:560` and `route.go:35`, loop
//! at `main.go:672`), and measure ~309ms together. The relay then serves any
//! number of `session` channels over the one tunnel it already established.
//!
//! Deliberately russh rather than `ControlMaster`. Multiplexing across
//! short-lived `ssh` invocations needs `ControlPersist`, and a socket file
//! outliving its TCP connection is exactly the failure that got ControlMaster
//! removed from this codebase (see `RelaySsh` in `commands/code.rs`): a dead
//! master left behind by a sleeping agent killed the next run with a bare exit
//! 255. In-process there is no socket and no cross-run state, so that failure
//! cannot occur rather than being mitigated.
//!
//! Covers the captured-output commands only. The interactive session keeps
//! using `ssh`, which already handles the pty, window resizes and signals.

use anyhow::{Context, Result, bail};
use russh::ChannelMsg;
use russh::client::{Config, Handle, Handler};
use std::sync::Arc;

use crate::config::Configs;

/// Result of one command run over the shared connection.
pub struct MuxOutput {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl MuxOutput {
    pub fn success(&self) -> bool {
        self.code == 0
    }

    pub fn stdout_utf8(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }
}

struct MuxHandler {
    host: &'static str,
    port: u16,
}

impl Handler for MuxHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Trust on first use against the relay's own known-hosts file, matching
        // `StrictHostKeyChecking=accept-new` plus `UserKnownHostsFile` on the
        // `ssh` path. The user's ~/.ssh/known_hosts stays untouched, which is a
        // deliberate property of the existing relay handling.
        let path = crate::commands::code::relay_known_hosts()?;
        match russh::keys::known_hosts::check_known_hosts_path(
            self.host,
            self.port,
            server_public_key,
            &path,
        ) {
            Ok(true) => Ok(true),
            Ok(false) => {
                russh::keys::known_hosts::learn_known_hosts_path(
                    self.host,
                    self.port,
                    server_public_key,
                    &path,
                )
                .with_context(|| format!("Failed to record the host key for {}", self.host))?;
                Ok(true)
            }
            Err(russh::keys::Error::KeyChanged { line }) => bail!(
                "The relay host key for {} does not match the one recorded in {} (line {line}). \
                 This can mean the relay's key was rotated, or that something is impersonating \
                 it. Verify the change before removing that line.",
                self.host,
                path.display(),
            ),
            Err(err) => Err(anyhow::Error::new(err)
                .context(format!("Failed to verify the host key for {}", self.host))),
        }
    }
}

/// A connected, authenticated SSH session to the relay, routed to one target.
///
/// The relay resolves the target once during authentication
/// (`RouteSSH(fingerprint, target)`, `main.go:560`), so a connection is bound to
/// a single agent for its lifetime. That suits a launch, where every command
/// goes to the same agent, and rules out batching work across agents.
pub struct RelayMux {
    session: Handle<MuxHandler>,
}

impl RelayMux {
    /// Connect and authenticate.
    ///
    /// Reaching this point at all means the relay routed the target, so a
    /// successful connect is itself the readiness signal a separate probe used
    /// to fetch with its own round trip.
    pub async fn connect(ssh_target: &str) -> Result<Self> {
        let (host, port) = Configs::get_ssh_relay();
        let port = port.unwrap_or(22);

        let mut session = russh::client::connect(
            Arc::new(Config::default()),
            (host, port),
            MuxHandler { host, port },
        )
        .await
        .with_context(|| format!("Failed to connect to the Railway relay at {host}:{port}"))?;

        // The target is the *username*: a relay target (`agent:<env>:<id>`) is
        // not a hostname, same as the `<target>@<relay>` form the ssh path uses.
        crate::controllers::ssh::authenticate(&mut session, ssh_target)
            .await
            .with_context(|| format!("Failed to authenticate to the relay as {ssh_target}"))?;

        Ok(Self { session })
    }

    /// Run one command on its own channel over the shared connection.
    ///
    /// `stdin` is written then EOF'd before reading, matching what the `ssh`
    /// path does with a payload. Both provision scripts read stdin first and a
    /// script that never drains it leaves the writer on a broken pipe.
    pub async fn exec(&self, command: &str, stdin: Option<&[u8]>) -> Result<MuxOutput> {
        let mut channel = self
            .session
            .channel_open_session()
            .await
            .context("Failed to open a channel on the relay connection")?;

        channel
            .exec(true, command)
            .await
            .context("Failed to start the remote command")?;

        if let Some(payload) = stdin {
            channel
                .data(payload)
                .await
                .context("Failed to send the command's stdin")?;
        }
        // Always, even with no payload: a script blocking on `cat` waits for EOF.
        channel.eof().await.context("Failed to close stdin")?;

        let mut out = MuxOutput {
            // No exit-status message is not the same as success. SSH servers may
            // close without one, and treating that as 0 is how a failed script
            // reads as a clean run.
            code: -1,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };

        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => out.stdout.extend_from_slice(data),
                ChannelMsg::ExtendedData { ref data, ext } => {
                    // ext 1 is stderr; anything else is not something we asked for.
                    if ext == 1 {
                        out.stderr.extend_from_slice(data);
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => out.code = exit_status as i32,
                ChannelMsg::Eof | ChannelMsg::Close => {}
                _ => {}
            }
        }

        Ok(out)
    }
}
