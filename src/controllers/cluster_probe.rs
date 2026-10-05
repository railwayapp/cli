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
//! container -- see [`HEALTH_API_AUTH_PRELUDE`] (curl) and
//! [`WGET_HEALTH_API_AUTH_PRELUDE`] (wget).
//!
//! Which HTTP client runs inside the container is the engine's declaration
//! ([`NodeHttpClient`] in the registry), never a guess: the data images do
//! not all ship the same one.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::config::HttpEndpoint;
use super::database_engines::NodeHttpClient;
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

/// The shell text that GETs a localhost endpoint inside the node's container
/// and reports the HTTP status it answered with, in `client`'s dialect.
///
/// curl prints the bare code (`-w '%{http_code}'`). GNU wget has no such
/// format, so it prints the server's response head (`-S`; `-q` silences
/// everything else) and [`parse_status_probe`] reads the status line out of
/// it. wget exits 8 on a non-2xx answer (6 on a 401), where `curl -s` exits
/// 0 on any answer -- both are mapped back to 0, so a 503 from the role
/// endpoint is the "not primary" verdict it means, while a transport failure
/// (exit 4, connection refused) still fails the exec the way curl's does.
/// `--tries=1` because wget otherwise retries a timed-out read 20 times;
/// `--no-config` so no wgetrc in the image changes the request.
fn status_probe_command(client: NodeHttpClient, endpoint: &ResolvedEndpoint) -> String {
    match client {
        NodeHttpClient::Curl => format!(
            "curl -s -o /dev/null --max-time 4 -w '%{{http_code}}' localhost:{}{}",
            endpoint.port, endpoint.path
        ),
        NodeHttpClient::Wget => format!(
            r#"wget -q -S -O /dev/null --timeout=4 --tries=1 --no-config http://localhost:{}{} 2>&1 || {{ rc=$?; [ "$rc" -eq 6 ] || [ "$rc" -eq 8 ] || exit "$rc"; }}"#,
            endpoint.port, endpoint.path
        ),
    }
}

/// Reads the HTTP status out of [`status_probe_command`]'s output.
fn parse_status_probe(client: NodeHttpClient, output: &str) -> Result<u16> {
    let parsed = match client {
        NodeHttpClient::Curl => output.trim().parse::<u16>().ok(),
        // `  HTTP/1.1 503 Service Unavailable`, one per response; after a
        // redirect the last one is the answer that counts.
        NodeHttpClient::Wget => output
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| line.starts_with("HTTP/"))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok()),
    };
    parsed.with_context(|| format!("Unexpected response from the node: {}", output.trim()))
}

/// GETs a localhost endpoint inside `instance_id`'s container with the
/// engine's declared HTTP client and returns the HTTP status it answered with.
async fn probe_status(
    client: NodeHttpClient,
    instance_id: &str,
    endpoint: &ResolvedEndpoint,
) -> Result<u16> {
    let command = status_probe_command(client, endpoint);
    let output = exec_probe_in_container(instance_id, &command, PROBE_TIMEOUT)
        .await
        .context("Probing the node failed")?;
    parse_status_probe(client, &output)
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
    client: NodeHttpClient,
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
                && let Ok(code) = probe_status(client, instance_id, endpoint).await
            {
                status.reachable = true;
                status.healthy = Some((200..300).contains(&code));
            }

            if let Some(endpoint) = role
                && let Ok(code) = probe_status(client, instance_id, endpoint).await
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

/// The wget twin of [`HEALTH_API_AUTH_PRELUDE`]: same credential, same
/// "blank password = open node" rule, delivered to GNU wget instead of curl.
///
/// wget has no config-from-stdin (`--config=/dev/stdin` on a pipe is refused:
/// "Exiting due to error in /dev/stdin"), and `--user`/`--password` would put
/// the secret in argv, which every process in the container can read. So the
/// credential goes into a wgetrc document in a `mktemp` file -- created `0600`
/// and owned by the exec's user, the same visibility the container already
/// gives `HEALTH_API_PASSWORD` itself through `/proc/<pid>/environ` -- and
/// argv carries only that file's path (`--config=<path>`). The file is
/// written by `printf`, a shell builtin, so no process is spawned with the
/// secret, and it is removed on exit (an `EXIT` trap, with `HUP`/`INT`/`TERM`
/// routed through it when the SSH session drops).
///
/// wgetrc has no quoting: a value is the rest of its line with surrounding
/// whitespace stripped, and `#`, `=`, `"`, `\`, `$` and `'` are literal in
/// it. Both data images' health servers trim the username and password
/// before comparing, so the shell trims them the same way first (`ha_trim`,
/// pure POSIX parameter expansion) -- an empty username after trimming falls
/// back to `railway` as the servers do. A line break left INSIDE either value
/// cannot be carried by wgetrc at all, so the exec refuses (exit 64, nothing
/// sent) rather than authenticate as some other string.
/// `auth_no_challenge = on` sends the credential preemptively, as `curl`
/// does, instead of waiting for the 401 challenge.
const WGET_HEALTH_API_AUTH_PRELUDE: &str = concat!(
    r#"ha_trim() { v=$1; v=${v#"${v%%[![:space:]]*}"}; v=${v%"${v##*[![:space:]]}"}; printf '%s' "$v"; }; "#,
    r#"HEALTH_API_PW=$(ha_trim "${HEALTH_API_PASSWORD:-}"); "#,
    r#"HEALTH_API_USER=$(ha_trim "${HEALTH_API_USERNAME:-}"); [ -n "$HEALTH_API_USER" ] || HEALTH_API_USER=railway; "#,
    r#"if [ -n "$HEALTH_API_PW" ]; then nl=$(printf '\nx'); nl=${nl%x}; cr=$(printf '\r'); "#,
    r#"case "$HEALTH_API_USER$HEALTH_API_PW" in *"$nl"*|*"$cr"*) echo 'HEALTH_API_USERNAME/HEALTH_API_PASSWORD contains a line break, which wget cannot send; refusing the request' >&2; exit 64;; esac; "#,
    r#"HEALTH_API_RC=$(mktemp) || exit 1; trap 'rm -f "$HEALTH_API_RC"' EXIT; trap 'exit 129' HUP INT TERM; "#,
    r#"printf 'auth_no_challenge = on\nhttp_user = %s\nhttp_password = %s\n' "$HEALTH_API_USER" "$HEALTH_API_PW" > "$HEALTH_API_RC"; "#,
    r#"set -- --config="$HEALTH_API_RC"; else set -- --no-config; fi; "#,
);

/// Turns wget's response head (on the pipe) into the `\nHTTP_STATUS:<code>`
/// trailer `curl -w` writes, so one parser reads both clients. The body goes
/// straight to stdout through fd 3 while the response head is read off
/// stderr; when no status line arrived at all, whatever wget said instead is
/// passed through as the diagnostic.
const WGET_STATUS_TRAILER: &str = concat!(
    r#"{ s=; o=; while IFS= read -r l; do l=${l#"${l%%[! ]*}"}; "#,
    r#"case $l in HTTP/[0-9]*) s=${l#* }; s=${s%% *};; *) o="$o$l ";; esac; done; "#,
    r#"if [ -n "$s" ]; then printf '\nHTTP_STATUS:%s' "$s"; else printf '%s' "$o"; fi; }"#,
);

/// The exact shell text the switchover runs, so a test can pin both halves:
/// the credential resolution and the request itself.
///
/// curl: the config document is piped into curl on every run; curl reads it
/// only when `$@` says `-K -`. wget: `$@` is `--config=<0600 file>` or
/// `--no-config`; `--content-on-error` keeps the coordinator's refusal body,
/// and `--timeout`/`--tries=1` bound the request as curl's `--max-time` does
/// (the exec's own deadline in [`request_switchover`] caps both).
fn switchover_command(client: NodeHttpClient, endpoint: &ResolvedEndpoint) -> String {
    match client {
        NodeHttpClient::Curl => format!(
            r#"{prelude}printf '%s\n' "$HEALTH_API_CFG" | curl -s --max-time 8 -w '\nHTTP_STATUS:%{{http_code}}' "$@" -X POST localhost:{port}{path}"#,
            prelude = HEALTH_API_AUTH_PRELUDE,
            port = endpoint.port,
            path = endpoint.path,
        ),
        NodeHttpClient::Wget => format!(
            r#"{prelude}{{ wget -q -S -O - --content-on-error --timeout=8 --tries=1 "$@" --method=POST http://localhost:{port}{path} 2>&1 >&3 | {trailer}; }} 3>&1"#,
            prelude = WGET_HEALTH_API_AUTH_PRELUDE,
            trailer = WGET_STATUS_TRAILER,
            port = endpoint.port,
            path = endpoint.path,
        ),
    }
}

/// Asks `instance_id`'s own colocated coordinator to make THAT node the
/// primary. A 2xx means the handoff was accepted -- never that it completed;
/// confirmation comes from the role endpoint flipping, which is the same
/// signal everything else reads. Anything else is the coordinator's own
/// refusal, surfaced with its body as the reason.
pub async fn request_switchover(
    client: NodeHttpClient,
    instance_id: &str,
    endpoint: &ResolvedEndpoint,
) -> Result<String> {
    let command = switchover_command(client, endpoint);

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
            .write_all(switchover_command(NodeHttpClient::Curl, &endpoint()).as_bytes())
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
        let cmd = switchover_command(NodeHttpClient::Curl, &endpoint());
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

    /// The exact text the curl dialect rendered before the client became a
    /// registry declaration. Engines that declare `curl` (MySQL) must keep
    /// running byte-for-byte the same commands.
    #[test]
    fn the_curl_dialect_renders_exactly_what_it_did_before() {
        let role = resolve(Some(&HttpEndpoint {
            port: Some(8080),
            path: Some("/role".to_string()),
        }))
        .unwrap();
        assert_eq!(
            status_probe_command(NodeHttpClient::Curl, &role),
            "curl -s -o /dev/null --max-time 4 -w '%{http_code}' localhost:8080/role"
        );
        assert_eq!(
            switchover_command(NodeHttpClient::Curl, &endpoint()),
            format!(
                r#"{HEALTH_API_AUTH_PRELUDE}printf '%s\n' "$HEALTH_API_CFG" | curl -s --max-time 8 -w '\nHTTP_STATUS:%{{http_code}}' "$@" -X POST localhost:8080/switchover"#
            )
        );
        assert_eq!(
            parse_status_probe(NodeHttpClient::Curl, "503").unwrap(),
            503
        );
    }

    #[test]
    fn the_wget_dialect_never_names_curl() {
        let role = resolve(Some(&HttpEndpoint {
            port: Some(8080),
            path: Some("/role".to_string()),
        }))
        .unwrap();
        let probe = status_probe_command(NodeHttpClient::Wget, &role);
        assert!(probe.starts_with("wget -q -S -O /dev/null --timeout=4 --tries=1 --no-config "));
        assert!(probe.contains("http://localhost:8080/role"));
        let switchover = switchover_command(NodeHttpClient::Wget, &endpoint());
        assert!(switchover.starts_with(WGET_HEALTH_API_AUTH_PRELUDE));
        for cmd in [&probe, &switchover] {
            assert!(!cmd.contains("curl"), "{cmd}");
            assert!(
                !cmd.contains("--password") && !cmd.contains("--user"),
                "{cmd}"
            );
        }
    }

    #[test]
    fn wget_status_is_read_off_the_last_response_line() {
        let head = "  HTTP/1.1 503 Service Unavailable\n  content-type: application/json\n  content-length: 21\n";
        assert_eq!(parse_status_probe(NodeHttpClient::Wget, head).unwrap(), 503);
        // A redirect prints one head per response; the final one answers.
        let redirected =
            "  HTTP/1.1 301 Moved Permanently\n  Location: /role/\n  HTTP/1.1 200 OK\n";
        assert_eq!(
            parse_status_probe(NodeHttpClient::Wget, redirected).unwrap(),
            200
        );
        // A header value that mentions HTTP/ is not a status line.
        let via = "  HTTP/1.1 200 OK\n  Via: HTTP/1.1 edge\n";
        assert_eq!(parse_status_probe(NodeHttpClient::Wget, via).unwrap(), 200);
        // Nothing answered: no status, which is "unreachable", never a code.
        assert!(parse_status_probe(NodeHttpClient::Wget, "").is_err());
        assert!(parse_status_probe(NodeHttpClient::Wget, "sh: 1: wget: not found").is_err());
    }

    /// A container stand-in whose `PATH` holds only what the shim directory
    /// provides: a `wget` shim (when `wget_script` is given), and the coreutils
    /// the commands use (`mktemp`, `rm`) linked from the host -- and, unless a
    /// test adds it, NO `curl`, exactly like the redis-ha and mongo-ha images.
    #[cfg(unix)]
    struct FakeNode {
        dir: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl FakeNode {
        fn new(tag: &str, wget_script: Option<&str>) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!(
                "cli-fake-node-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            for tool in ["mktemp", "rm"] {
                let real = ["/usr/bin", "/bin"]
                    .iter()
                    .map(|d| std::path::Path::new(d).join(tool))
                    .find(|p| p.exists())
                    .unwrap();
                std::os::unix::fs::symlink(real, dir.join("bin").join(tool)).unwrap();
            }
            if let Some(script) = wget_script {
                let shim = dir.join("bin").join("wget");
                std::fs::write(&shim, script).unwrap();
                std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self { dir }
        }

        /// Runs `command` through `/bin/sh -s`, as the SSH exec does.
        fn run(&self, command: &str, env: &[(&str, &str)]) -> std::process::Output {
            use std::io::Write;
            let mut child = std::process::Command::new("/bin/sh")
                .arg("-s")
                .env_clear()
                .env("PATH", self.dir.join("bin"))
                .env("TMPDIR", &self.dir)
                .env("SHIM_LOG", self.dir.join("wget.log"))
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
                .write_all(command.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        }

        fn log(&self) -> String {
            std::fs::read_to_string(self.dir.join("wget.log")).unwrap_or_default()
        }

        /// Files left behind in the node's temp dir (the credential file must
        /// not be one of them).
        fn leftovers(&self) -> Vec<String> {
            std::fs::read_dir(&self.dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n != "bin" && n != "wget.log")
                .collect()
        }
    }

    #[cfg(unix)]
    impl Drop for FakeNode {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A GNU wget stand-in: records argv (and, for `--config=<file>`, the
    /// file's permissions and content) to `$SHIM_LOG`, writes the response
    /// head to stderr as `-S` does, the body to stdout, and exits with
    /// wget's code for that status.
    #[cfg(unix)]
    fn wget_shim(status_line: &str, body: &str, exit: u8) -> String {
        format!(
            concat!(
                "#!/bin/sh\n",
                "for a in \"$@\"; do\n",
                "  printf 'ARG:%s\\n' \"$a\" >> \"$SHIM_LOG\"\n",
                "  case $a in --config=*) f=${{a#--config=}};\n",
                "    perm=$(/bin/ls -l \"$f\"); printf 'PERM:%.10s\\n' \"$perm\" >> \"$SHIM_LOG\";\n",
                "    while IFS= read -r line; do printf 'CFG:%s\\n' \"$line\" >> \"$SHIM_LOG\"; done < \"$f\";;\n",
                "  esac\n",
                "done\n",
                "printf '  %s\\n  Content-Type: text/plain\\n' '{status}' >&2\n",
                "printf '%s' '{body}'\n",
                "exit {exit}\n",
            ),
            status = status_line,
            body = body,
            exit = exit,
        )
    }

    /// The redis-ha image ships `wget` and no `curl`. Probing it with the
    /// client the registry declares for Redis has to reach the node -- with
    /// `curl` hard-coded (the defect) this exits 127 and every member reads
    /// as unreachable.
    #[cfg(unix)]
    #[test]
    fn the_redis_declaration_probes_with_a_client_its_image_ships() {
        use crate::controllers::database_engines::{REDIS, SwitchoverMechanism};
        let SwitchoverMechanism::DeclaredHttp { http_client } = REDIS.ha.unwrap().switchover else {
            panic!("redis-ha speaks the declared per-node contract");
        };
        let node = FakeNode::new(
            "redis-probe",
            Some(&wget_shim("HTTP/1.1 503 Service Unavailable", "", 8)),
        );
        let role = resolve(Some(&HttpEndpoint {
            port: Some(8080),
            path: Some("/role".to_string()),
        }))
        .unwrap();
        let out = node.run(&status_probe_command(http_client, &role), &[]);
        assert!(
            out.status.success(),
            "exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        let code = parse_status_probe(http_client, &String::from_utf8_lossy(&out.stdout)).unwrap();
        // 503 is the role contract's "not the primary" -- a verdict, which
        // must survive wget's non-zero exit on a non-2xx answer.
        assert_eq!(interpret_role(code), Some(false));
        assert!(node.log().contains("ARG:http://localhost:8080/role"));
    }

    #[cfg(unix)]
    #[test]
    fn a_wget_transport_failure_still_fails_the_exec() {
        // wget exits 4 on a refused connection and prints no response head:
        // that is "unreachable", the same as curl failing to connect.
        let node = FakeNode::new("wget-refused", Some("#!/bin/sh\nexit 4\n"));
        let out = node.run(
            &status_probe_command(NodeHttpClient::Wget, &endpoint()),
            &[],
        );
        assert_eq!(out.status.code(), Some(4));
    }

    #[cfg(unix)]
    #[test]
    fn the_redis_switchover_runs_on_an_image_that_ships_only_wget() {
        use crate::controllers::database_engines::{REDIS, SwitchoverMechanism};
        let SwitchoverMechanism::DeclaredHttp { http_client } = REDIS.ha.unwrap().switchover else {
            panic!("redis-ha speaks the declared per-node contract");
        };
        let node = FakeNode::new(
            "redis-switchover",
            Some(&wget_shim("HTTP/1.1 202 Accepted", "accepted", 0)),
        );
        let out = node.run(&switchover_command(http_client, &endpoint()), &[]);
        assert!(
            out.status.success(),
            "exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            parse_switchover_response(&String::from_utf8_lossy(&out.stdout)).unwrap(),
            "accepted"
        );
        let log = node.log();
        // An open node: no credential file, and wget told to read no config.
        assert!(log.contains("ARG:--no-config"), "{log}");
        assert!(!log.contains("CFG:"), "{log}");
        assert!(log.contains("ARG:--method=POST"), "{log}");
        assert!(
            log.contains("ARG:http://localhost:8080/switchover"),
            "{log}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_wget_refusal_surfaces_the_coordinators_body() {
        let node = FakeNode::new(
            "wget-refusal",
            Some(&wget_shim(
                "HTTP/1.1 409 Conflict",
                "candidate is not in sync",
                8,
            )),
        );
        let out = node.run(
            &switchover_command(NodeHttpClient::Wget, &endpoint()),
            &[("HEALTH_API_PASSWORD", "s3cret")],
        );
        assert!(out.status.success());
        let err = parse_switchover_response(&String::from_utf8_lossy(&out.stdout))
            .unwrap_err()
            .to_string();
        assert!(err.contains("409"), "{err}");
        assert!(err.contains("candidate is not in sync"), "{err}");
    }

    /// The credential reaches wget through a 0600 wgetrc file named in argv,
    /// never through argv itself, and the file is gone once the exec ends.
    /// wgetrc has no quoting, so the shim reading the file back line by line
    /// is what wget reads: `key = <rest of line, trimmed>`.
    #[cfg(unix)]
    #[test]
    fn the_wget_credential_travels_in_a_private_file_never_in_argv() {
        let password = "p\"a\\s$s' w#rd=x";
        let node = FakeNode::new(
            "wget-credential",
            Some(&wget_shim("HTTP/1.1 202 Accepted", "ok", 0)),
        );
        let out = node.run(
            &switchover_command(NodeHttpClient::Wget, &endpoint()),
            // Surrounding whitespace is trimmed by the health servers; the
            // shell trims the same way before writing the file.
            &[("HEALTH_API_PASSWORD", &format!("  {password}\n"))],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let log = node.log();
        let args: Vec<&str> = log.lines().filter_map(|l| l.strip_prefix("ARG:")).collect();
        assert!(
            args.iter()
                .all(|a| !a.contains("w#rd") && !a.contains("s3cret")),
            "{args:?}"
        );
        assert!(args.iter().any(|a| a.starts_with("--config=")), "{args:?}");
        assert!(log.contains("PERM:-rw-------"), "{log}");
        let cfg: Vec<&str> = log.lines().filter_map(|l| l.strip_prefix("CFG:")).collect();
        assert_eq!(
            cfg,
            vec![
                "auth_no_challenge = on",
                "http_user = railway",
                &format!("http_password = {password}"),
            ]
        );
        assert!(node.leftovers().is_empty(), "{:?}", node.leftovers());
    }

    #[cfg(unix)]
    #[test]
    fn the_wget_credential_honours_an_explicit_username() {
        let node = FakeNode::new(
            "wget-username",
            Some(&wget_shim("HTTP/1.1 202 Accepted", "ok", 0)),
        );
        let out = node.run(
            &switchover_command(NodeHttpClient::Wget, &endpoint()),
            &[
                ("HEALTH_API_PASSWORD", "s3cret"),
                ("HEALTH_API_USERNAME", " ops "),
            ],
        );
        assert!(out.status.success());
        let log = node.log();
        assert!(log.contains("CFG:http_user = ops\n"), "{log}");
        assert!(log.contains("CFG:http_password = s3cret\n"), "{log}");
    }

    /// wgetrc cannot carry a line break inside a value. Sending what is left
    /// of the password would authenticate as a different string, so nothing
    /// is sent at all.
    #[cfg(unix)]
    #[test]
    fn a_credential_wget_cannot_carry_is_refused_before_any_request() {
        let node = FakeNode::new(
            "wget-linebreak",
            Some(&wget_shim("HTTP/1.1 202 Accepted", "ok", 0)),
        );
        let out = node.run(
            &switchover_command(NodeHttpClient::Wget, &endpoint()),
            &[("HEALTH_API_PASSWORD", "first\nsecond")],
        );
        assert_eq!(out.status.code(), Some(64));
        assert!(String::from_utf8_lossy(&out.stderr).contains("line break"));
        assert!(node.log().is_empty(), "wget must not run: {}", node.log());
        assert!(node.leftovers().is_empty(), "{:?}", node.leftovers());
    }
}
