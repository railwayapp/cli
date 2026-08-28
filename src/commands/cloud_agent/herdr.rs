//! Report the pane's cloud-agent session to Herdr, when this process runs
//! inside one of its panes.
//!
//! Herdr (herdr.dev) is a terminal workspace manager for coding agents: it
//! records which agent conversation each pane holds and, after a restart,
//! relaunches the pane back into that conversation. Its integrations for
//! local agents report over a unix socket the pane's environment names —
//! `HERDR_ENV=1`, `HERDR_SOCKET_PATH`, `HERDR_PANE_ID` — with one
//! `pane.report_agent_session` request per change. A `railway code` session
//! is invisible to that machinery today: the harness runs on the VM, where
//! the socket does not exist, so the pane records nothing and a restore
//! lands on a bare shell.
//!
//! This module is the local half of the fix: the CLI itself reports the
//! `--resume` reference (`<agent id>:<durable session name>`) for the session
//! its pane is showing, under the agent label `railway-code`. Everything is
//! best-effort and silent — a report is a convenience for the workspace
//! manager, and no session should ever fail over one. Herdr 0.8.0
//! acknowledges these reports but does not yet persist agent kinds outside
//! its built-in set; reporting the honest label anyway is what its side of
//! the integration will key on.

use std::path::PathBuf;
use std::sync::OnceLock;

/// The pane contract herdr sets in every pane's environment. Present means
/// this process is running inside a herdr pane and the server is listening
/// on the socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneEnv {
    socket_path: PathBuf,
    pane_id: String,
}

/// The pane environment, read once — it cannot change within a process's
/// lifetime, and the callers that consult it do so from a render loop.
pub fn pane_env() -> Option<&'static PaneEnv> {
    static ENV: OnceLock<Option<PaneEnv>> = OnceLock::new();
    ENV.get_or_init(|| pane_env_from(|key| std::env::var(key).ok()))
        .as_ref()
}

/// [`pane_env`] over an arbitrary variable source, so the gating is checked
/// by tests rather than by mutating the test process's real environment.
fn pane_env_from(var: impl Fn(&str) -> Option<String>) -> Option<PaneEnv> {
    if var("HERDR_ENV").as_deref() != Some("1") {
        return None;
    }
    let socket_path = var("HERDR_SOCKET_PATH").filter(|v| !v.is_empty())?;
    let pane_id = var("HERDR_PANE_ID").filter(|v| !v.is_empty())?;
    Some(PaneEnv {
        socket_path: PathBuf::from(socket_path),
        pane_id,
    })
}

/// The reference `railway code --resume` takes, in the one shape both sides
/// agree on. The agent half is the id rather than the name: the reference is
/// stored by a machine and replayed cold, where a name's ambiguity has no one
/// to ask.
pub fn session_reference(agent_id: &str, session_name: &str) -> String {
    format!("{agent_id}:{session_name}")
}

/// Report the pane's current session, fire-and-forget.
///
/// Detached because every caller is on a path a user is watching — the render
/// loop, the moment before an ssh attach — and a local socket that has gone
/// away must cost nothing. Failures are silent for the same reason the hook
/// integrations' are: there is nothing actionable to say.
pub fn report_session_detached(env: &PaneEnv, reference: String) {
    let env = env.clone();
    tokio::task::spawn_blocking(move || {
        let _ = send_report(&env, &reference);
    });
}

/// One `pane.report_agent_session` request over the pane's socket: a single
/// JSON line out, one reply line read and discarded. The `seq` orders
/// reports on herdr's side, so a stale one can never overwrite a newer one.
#[cfg(unix)]
fn send_report(env: &PaneEnv, reference: &str) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let seq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default();
    let request = report_request(&env.pane_id, reference, seq);

    let mut stream = UnixStream::connect(&env.socket_path)?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(request.as_bytes())?;
    // The reply is not checked, only drained: herdr acks unconditionally, and
    // there is no retry that would make sense on a nack.
    let mut reply = [0u8; 1024];
    let _ = stream.read(&mut reply);
    Ok(())
}

/// Windows herdr speaks a different transport; until this module learns it,
/// the report is skipped rather than guessed at.
#[cfg(not(unix))]
fn send_report(_env: &PaneEnv, _reference: &str) -> std::io::Result<()> {
    Ok(())
}

/// The request line, newline-terminated the way the socket protocol frames
/// requests. Split from the send so the shape is pinned by a test.
fn report_request(pane_id: &str, reference: &str, seq: u64) -> String {
    let request = serde_json::json!({
        "id": format!("railway:cli:{seq}"),
        "method": "pane.report_agent_session",
        "params": {
            "pane_id": pane_id,
            "source": "railway:cli",
            "agent": "railway-code",
            "seq": seq,
            "agent_session_id": reference,
        },
    });
    format!("{request}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn the_pane_contract_needs_all_three_variables() {
        let full = &[
            ("HERDR_ENV", "1"),
            ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
            ("HERDR_PANE_ID", "w1:p2"),
        ];
        let env = pane_env_from(env_of(full)).unwrap();
        assert_eq!(env.pane_id, "w1:p2");
        assert_eq!(env.socket_path, PathBuf::from("/tmp/herdr.sock"));

        // Anything less than the full contract means "not a herdr pane" —
        // reporting into a socket that was inherited from somewhere else
        // would label a pane herdr never asked about.
        for missing in ["HERDR_ENV", "HERDR_SOCKET_PATH", "HERDR_PANE_ID"] {
            let partial: Vec<_> = full
                .iter()
                .filter(|(k, _)| *k != missing)
                .copied()
                .collect();
            assert!(
                pane_env_from(env_of(&partial)).is_none(),
                "{missing} absent should gate the report off"
            );
        }
        let off = &[
            ("HERDR_ENV", "0"),
            ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
            ("HERDR_PANE_ID", "w1:p2"),
        ];
        assert!(pane_env_from(env_of(off)).is_none());
    }

    #[test]
    fn the_request_is_one_framed_line_herdr_understands() {
        let line = report_request("w3:p7", "agent-id:claude-x3k9f2", 42);
        assert!(line.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["method"], "pane.report_agent_session");
        let params = &parsed["params"];
        assert_eq!(params["pane_id"], "w3:p7");
        assert_eq!(params["agent"], "railway-code");
        assert_eq!(params["source"], "railway:cli");
        assert_eq!(params["seq"], 42);
        assert_eq!(params["agent_session_id"], "agent-id:claude-x3k9f2");
    }

    /// End to end over a real socket: what a herdr server would read is the
    /// framed request, and a slow or dead server never propagates an error.
    #[cfg(unix)]
    #[test]
    fn a_report_reaches_a_listening_socket_and_a_dead_one_is_silent() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("railway-herdr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("herdr.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut stream = &stream;
            stream
                .write_all(b"{\"id\":\"x\",\"result\":{\"type\":\"ok\"}}\n")
                .unwrap();
            line
        });

        let env = PaneEnv {
            socket_path: socket_path.clone(),
            pane_id: "w3:p7".into(),
        };
        send_report(&env, "agent-id:claude-x3k9f2").unwrap();
        let received = server.join().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(received.trim()).unwrap();
        assert_eq!(
            parsed["params"]["agent_session_id"],
            "agent-id:claude-x3k9f2"
        );

        // Socket gone: the error stays inside `send_report`'s Result, and the
        // detached caller drops it — nothing to assert beyond "is an Err".
        std::fs::remove_file(&socket_path).unwrap();
        assert!(send_report(&env, "agent-id:claude-x3k9f2").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
