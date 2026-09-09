//! The platform's generic cluster probe contract, for topologies whose data
//! nodes expose plain HTTP rather than a coordinator API the CLI speaks.
//!
//! A template declares coordinates -- `clusterWiring.dataNodeHealthCheck`,
//! `dataNodeRoleCheck`, `dataNodeSwitchover` -- and the CLI probes exactly
//! what it is pointed at, carrying no idea what is listening on the other end.
//! How a node decides it is the primary, or promotes itself (Sentinel's
//! priority-biased election, Group Replication's set-primary UDF), is
//! implemented by the template's own image behind these endpoints.
//!
//! Transport is the same one [`super::patroni`] uses: the endpoints listen on
//! localhost inside each member's own container, so an SSH exec into the
//! container reaches them with no port-forwarding. The mutating endpoint is
//! gated by the node's own `HEALTH_API_PASSWORD`, resolved inside that same
//! container -- see [`HEALTH_API_AUTH_PRELUDE`].

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::config::HttpEndpoint;
use super::exec::{exec_in_container, exec_probe_in_container};

/// Per-node probe timeout, matching the Patroni client's: keeps `status`
/// responsive against a wedged member instead of hanging on it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolved coordinates of a declared endpoint -- a template may declare the
/// object with either half missing, which is not something to probe against.
pub struct ResolvedEndpoint {
    pub port: i64,
    pub path: String,
}

pub fn resolve(endpoint: Option<&HttpEndpoint>) -> Option<ResolvedEndpoint> {
    let endpoint = endpoint?;
    let port = endpoint.port?;
    let path = endpoint.path.clone()?;
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    Some(ResolvedEndpoint { port, path })
}

/// What a data node's role endpoint said about itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeStatus {
    /// The node's own container answered at all.
    pub reachable: bool,
    /// `Some(true)` when the node's coordinator currently treats it as the
    /// primary, `Some(false)` when it explicitly does not, `None` when the
    /// answer was neither -- an unrecognized status is unknown, never a
    /// silent "no".
    pub is_primary: Option<bool>,
    /// The health endpoint returned 2xx.
    pub healthy: Option<bool>,
}

/// Runs `curl` against a localhost endpoint inside `instance_id`'s container
/// and returns the HTTP status it answered with.
async fn probe_status(instance_id: &str, endpoint: &ResolvedEndpoint) -> Result<u16> {
    let command = format!(
        "curl -s -o /dev/null --max-time 4 -w '%{{http_code}}' localhost:{}{}",
        endpoint.port, endpoint.path
    );
    let output = exec_probe_in_container(instance_id, &command, PROBE_TIMEOUT)
        .await
        .context("Probing the node failed")?;
    output
        .trim()
        .parse::<u16>()
        .with_context(|| format!("Unexpected response from the node: {}", output.trim()))
}

/// Interprets the declared role contract: 200 means this node is the one its
/// own coordinator currently treats as primary, 503 means it is not, anything
/// else is unknown.
fn interpret_role(status: u16) -> Option<bool> {
    match status {
        200 => Some(true),
        503 => Some(false),
        _ => None,
    }
}

/// Probes every data node's own container independently -- so a partition
/// that leaves one node unable to reach the rest shows up as specifically
/// THAT node being unreachable, rather than being masked by a healthy
/// neighbour's answer.
///
/// Results are keyed by Railway service id so callers can join them back
/// against the cluster's membership. An unreachable node degrades to
/// `NodeStatus::default()` rather than failing the whole probe.
pub async fn probe_nodes(
    instance_ids: &BTreeMap<String, String>,
    health: Option<&HttpEndpoint>,
    role: Option<&HttpEndpoint>,
) -> BTreeMap<String, NodeStatus> {
    let health = resolve(health);
    let role = resolve(role);

    let probes = instance_ids.iter().map(|(service_id, instance_id)| {
        let health = health.as_ref();
        let role = role.as_ref();
        async move {
            let mut status = NodeStatus::default();

            if let Some(endpoint) = health
                && let Ok(code) = probe_status(instance_id, endpoint).await
            {
                status.reachable = true;
                status.healthy = Some((200..300).contains(&code));
            }

            if let Some(endpoint) = role
                && let Ok(code) = probe_status(instance_id, endpoint).await
            {
                status.reachable = true;
                status.is_primary = interpret_role(code);
            }

            (service_id.clone(), status)
        }
    });

    futures::future::join_all(probes)
        .await
        .into_iter()
        .collect()
}

/// Credential for the node's health server, resolved INSIDE the container the
/// way the image resolves it: `HEALTH_API_PASSWORD` gates the mutating routes
/// (blank = open) and `HEALTH_API_USERNAME` defaults to `railway`. Stages it
/// for curl as a one-line config document in `$HEALTH_API_CFG`
/// (`user = "user:pass"`) with `$@` holding `-K -`, which makes curl read
/// that document from stdin -- or leaves both empty when the node carries no
/// password. An open node ignores the header and an enforcing one requires
/// it, so one command spans a cluster mid-rollout, and no secret enters the
/// exec payload, this process, or a log line.
///
/// Stdin rather than `-u user:pass` because argv is public inside the
/// container: `/proc/<pid>/cmdline` (`ps`) shows every process's arguments
/// to every other process in the PID namespace for as long as the request
/// runs. `printf` is a builtin of the shells the data images ship (dash,
/// bash), so the pipe spawns no process carrying the secret either.
///
/// `curl_cfg_quote` escapes a string for a double-quoted value in curl's
/// config syntax the way curl's parser reads it back: `\` and `"` are
/// backslashed and a newline becomes `\n`, since a raw newline would end
/// the value. Everything else (`$`, `'`, `#`, whitespace) is literal inside
/// the quotes. Pure POSIX sh, so it asks nothing of the image beyond the
/// shell itself: `s` is the unread rest of the input, `c` the character in
/// hand, `r` the input after it, `q` the escaped output. Twin of the Patroni
/// prelude in [`super::patroni`].
const HEALTH_API_AUTH_PRELUDE: &str = concat!(
    r#"HEALTH_API_PW="${HEALTH_API_PASSWORD:-}"; "#,
    r#"HEALTH_API_USER="${HEALTH_API_USERNAME:-railway}"; "#,
    r#"curl_cfg_quote() { s=$1; q=; nl=$(printf '\nx'); nl=${nl%x}; "#,
    r#"while [ -n "$s" ]; do r=${s#?}; c=${s%"$r"}; s=$r; "#,
    r#"case $c in \\) q="$q\\\\";; \") q="$q\\\"";; "$nl") q="$q\\n";; *) q="$q$c";; esac; done; "#,
    r#"printf '%s' "$q"; }; "#,
    r#"if [ -n "$HEALTH_API_PW" ]; then HEALTH_API_CFG="user = \"$(curl_cfg_quote "$HEALTH_API_USER:$HEALTH_API_PW")\""; set -- -K -; else HEALTH_API_CFG=; set --; fi; "#,
);

/// The exact shell text the switchover runs, so a test can pin both halves:
/// the credential resolution and the request itself. The config document is
/// piped into curl on every run; curl reads it only when `$@` says `-K -`.
fn switchover_command(endpoint: &ResolvedEndpoint) -> String {
    format!(
        r#"{prelude}printf '%s\n' "$HEALTH_API_CFG" | curl -s --max-time 8 -w '\nHTTP_STATUS:%{{http_code}}' "$@" -X POST localhost:{port}{path}"#,
        prelude = HEALTH_API_AUTH_PRELUDE,
        port = endpoint.port,
        path = endpoint.path,
    )
}

/// Asks `instance_id`'s own colocated coordinator to make THAT node the
/// primary. A 2xx means the handoff was accepted -- never that it completed;
/// confirmation comes from the role endpoint flipping, which is the same
/// signal everything else reads. Anything else is the coordinator's own
/// refusal, surfaced with its body as the reason.
pub async fn request_switchover(instance_id: &str, endpoint: &ResolvedEndpoint) -> Result<String> {
    let command = switchover_command(endpoint);

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        exec_in_container(instance_id, &command),
    )
    .await
    .context("Timed out requesting switchover")??;

    parse_switchover_response(&output)
}

/// Splits `curl -w`'s `<body>\nHTTP_STATUS:<code>` shape and turns a non-2xx
/// or unparseable status into an error carrying the coordinator's own
/// response body, which is what explains WHY a handoff was refused.
fn parse_switchover_response(output: &str) -> Result<String> {
    let (body, status) = match output.rsplit_once("HTTP_STATUS:") {
        Some((body, status)) => (body.trim().to_string(), status.trim().parse::<u16>().ok()),
        None => (output.trim().to_string(), None),
    };

    match status {
        Some(200..=299) => Ok(body),
        Some(code) => bail!("The node refused the switchover ({code}): {body}"),
        None => bail!("The switchover request returned an unexpected response: {body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_requires_both_halves_and_normalizes_the_path() {
        let resolved = resolve(Some(&HttpEndpoint {
            port: Some(8080),
            path: Some("/role".to_string()),
        }))
        .unwrap();
        assert_eq!(resolved.port, 8080);
        assert_eq!(resolved.path, "/role");

        // A path authored without its leading slash still probes the right URL.
        let resolved = resolve(Some(&HttpEndpoint {
            port: Some(8080),
            path: Some("role".to_string()),
        }))
        .unwrap();
        assert_eq!(resolved.path, "/role");

        assert!(resolve(None).is_none());
        assert!(
            resolve(Some(&HttpEndpoint {
                port: Some(8080),
                path: None
            }))
            .is_none()
        );
        assert!(
            resolve(Some(&HttpEndpoint {
                port: None,
                path: Some("/role".to_string())
            }))
            .is_none()
        );
    }

    #[test]
    fn role_contract_treats_anything_unrecognized_as_unknown() {
        assert_eq!(interpret_role(200), Some(true));
        assert_eq!(interpret_role(503), Some(false));
        // A 500 is the sidecar failing, not a demotion -- reporting it as
        // "not the primary" would invent a fact the node never stated.
        assert_eq!(interpret_role(500), None);
        assert_eq!(interpret_role(404), None);
    }

    #[test]
    fn switchover_accepts_2xx_and_surfaces_a_refusal_verbatim() {
        assert_eq!(
            parse_switchover_response("accepted\nHTTP_STATUS:200").unwrap(),
            "accepted"
        );

        let err =
            parse_switchover_response("cannot promote: candidate is not in sync\nHTTP_STATUS:409")
                .unwrap_err()
                .to_string();
        assert!(err.contains("409"));
        assert!(err.contains("candidate is not in sync"));

        let err = parse_switchover_response("curl: (7) connection refused")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unexpected response"));
    }
    fn endpoint() -> ResolvedEndpoint {
        resolve(Some(&HttpEndpoint {
            port: Some(8080),
            path: Some("/switchover".to_string()),
        }))
        .unwrap()
    }

    /// What the `curl` shim saw: its argv, one entry per element, and the
    /// config document it was handed on stdin when that argv said `-K -`.
    #[cfg(unix)]
    struct CurlCall {
        argv: Vec<String>,
        config: Option<String>,
    }

    #[cfg(unix)]
    /// Runs the emitted switchover text through a real `sh`, with a `curl`
    /// shim that prints its argv one per line and, when told to read a
    /// config from stdin, that config too -- so the assertions are about
    /// what curl is actually handed rather than about string shapes. The
    /// command only ever runs inside a Linux container, and Windows has no
    /// shell to check it against --
    /// `switchover_command_resolves_the_credential_inside_the_container`
    /// covers what can be asserted everywhere.
    fn curl_call_for(env: &[(&str, &str)]) -> CurlCall {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "cli-health-api-auth-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("curl");
        std::fs::write(
            &shim,
            concat!(
                "#!/bin/sh\n",
                "printf '%s\\n' \"$@\"\n",
                "prev=\n",
                "for a in \"$@\"; do\n",
                "  if [ \"$prev\" = -K ] && [ \"$a\" = - ]; then printf '%s\\n' '@@CONFIG@@'; cat; fi\n",
                "  prev=$a\n",
                "done\n",
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut child = std::process::Command::new("sh")
            .arg("-s")
            // The shim must WIN over a real curl, but `sh` itself still has to
            // be findable -- replacing PATH outright makes the spawn fail.
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("HEALTH_API_PASSWORD")
            .env_remove("HEALTH_API_USERNAME")
            .envs(env.iter().copied())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(switchover_command(&endpoint()).as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        let (argv, config) = match stdout.split_once("@@CONFIG@@\n") {
            Some((argv, config)) => (argv.to_string(), Some(config.to_string())),
            None => (stdout, None),
        };
        CurlCall {
            argv: argv.lines().map(str::to_string).collect(),
            config,
        }
    }

    /// How curl's config parser (`unslashquote` in `src/tool_parsecfg.c`)
    /// reads a double-quoted value: `\\`, `\"`, `\t`, `\n`, `\r` and `\v`
    /// are escapes, a backslash before anything else is dropped, and the
    /// value ends at the closing quote. Pins that the shell-side escaper
    /// speaks the dialect curl actually parses.
    #[cfg(unix)]
    fn curl_config_value(line: &str) -> String {
        let (_, quoted) = line.split_once('"').expect("a double-quoted value");
        let mut out = String::new();
        let mut chars = quoted.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('t') => out.push('\t'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('v') => out.push('\u{0B}'),
                    Some(other) => out.push(other),
                    None => break,
                },
                c => out.push(c),
            }
        }
        out
    }

    #[test]
    fn switchover_command_resolves_the_credential_inside_the_container() {
        let cmd = switchover_command(&endpoint());
        assert!(cmd.starts_with(HEALTH_API_AUTH_PRELUDE));
        assert!(cmd.contains("${HEALTH_API_PASSWORD:-}"));
        assert!(cmd.contains("${HEALTH_API_USERNAME:-railway}"));
        assert!(cmd.contains(
            r#"HEALTH_API_CFG="user = \"$(curl_cfg_quote "$HEALTH_API_USER:$HEALTH_API_PW")\""; set -- -K -"#
        ));
        assert!(cmd.contains("else HEALTH_API_CFG=; set --; fi"));
        assert!(
            !cmd.contains(" -u "),
            "the credential must never be a curl argument"
        );
        // The config document is piped into curl, ahead of the request.
        assert!(cmd.contains(r#"printf '%s\n' "$HEALTH_API_CFG" | curl "#));
        assert!(cmd.ends_with(r#""$@" -X POST localhost:8080/switchover"#));
    }

    #[cfg(unix)]
    #[test]
    fn an_open_node_gets_no_credential_flag_at_all() {
        let call = curl_call_for(&[]);
        // No password in the container => no config document and no `-K` at
        // all; `user = "railway:"` would be a malformed credential rather
        // than "none".
        assert!(call.config.is_none(), "{:?}", call.config);
        assert!(
            !call.argv.iter().any(|a| a == "-K" || a == "-u"),
            "{:?}",
            call.argv
        );
        assert_eq!(call.argv.last().unwrap(), "localhost:8080/switchover");
    }

    #[cfg(unix)]
    #[test]
    fn a_password_alone_authenticates_as_the_default_user() {
        let call = curl_call_for(&[("HEALTH_API_PASSWORD", "s3cret")]);
        assert_eq!(call.config.as_deref(), Some("user = \"railway:s3cret\"\n"));
        // argv is public inside the container; the credential never rides it.
        assert!(
            call.argv.iter().all(|a| !a.contains("s3cret")),
            "{:?}",
            call.argv
        );
        assert!(!call.argv.iter().any(|a| a == "-u"), "{:?}", call.argv);
        // curl reads the config ahead of the request itself.
        let k = call
            .argv
            .iter()
            .position(|a| a == "-K")
            .expect("curl told to read its config from stdin");
        assert_eq!(call.argv[k + 1], "-");
        assert!(call.argv.iter().position(|a| a == "-X").unwrap() > k);
    }

    #[cfg(unix)]
    #[test]
    fn an_explicit_username_wins_over_the_default() {
        let call = curl_call_for(&[
            ("HEALTH_API_PASSWORD", "s3cret"),
            ("HEALTH_API_USERNAME", "ops"),
        ]);
        assert_eq!(call.config.as_deref(), Some("user = \"ops:s3cret\"\n"));
    }

    /// A password is arbitrary text, and curl's config parser gives `\` and
    /// `"` meaning inside a double-quoted value and ends the value at a
    /// newline -- so the shell escapes exactly those, and what curl reads
    /// back (`curl_config_value`, its parser's rules) is the original.
    #[cfg(unix)]
    #[test]
    fn the_credential_is_escaped_for_curls_config_parser() {
        let password = "p\"a\\s$s' w#rd\nnext\ttab";
        let call = curl_call_for(&[("HEALTH_API_PASSWORD", password)]);
        let config = call.config.expect("config document piped to curl");
        assert_eq!(
            config,
            "user = \"railway:p\\\"a\\\\s$s' w#rd\\nnext\ttab\"\n"
        );
        assert_eq!(curl_config_value(&config), format!("railway:{password}"));
        assert!(
            call.argv.iter().all(|a| !a.contains("w#rd")),
            "{:?}",
            call.argv
        );
    }
}
