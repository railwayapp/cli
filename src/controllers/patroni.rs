//! Live Patroni REST API probe/switchover, reached the same way the
//! frontend reaches it from the browser (a tunnel into the same container),
//! except the CLI already has a working server-side equivalent:
//! `controllers::exec::exec_in_container`. There is no GraphQL mutation for
//! any of this -- Patroni's REST API listens on `localhost:8008` inside
//! every cluster member's own container, so no port-forwarding is needed
//! once we're SSH'd in.
//!
//! Patroni member names are stamped from the cluster wiring's
//! `replicaNodeNameVariable`/legacy `PATRONI_NAME` as the service's own
//! lowercased name (see `template_apply::restamp_after_replica_adjust` and
//! `cluster_scale`'s live-scale equivalent) -- so matching a Railway service
//! to its Patroni member is always a case-insensitive name comparison, never
//! an id lookup.

use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::exec::{exec_in_container, exec_probe_in_container};
use super::project::{ServiceContext, find_service_instance, get_environment_instances};

/// Per-member probe/switchover timeout. Keeps `status`/`switchover`
/// responsive against an unreachable or wedged member instead of hanging.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A single member entry from Patroni's `GET /cluster` response. Every field
/// is optional/defaulted -- this is a best-effort live probe, not a
/// contract, and a Patroni version quirk or partial response should degrade
/// gracefully rather than fail the whole probe.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PatroniMember {
    pub name: String,
    pub role: String,
    pub state: String,
    /// Streaming replication lag in bytes, present on replicas only.
    pub lag: Option<serde_json::Value>,
    pub timeline: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PatroniClusterResponse {
    members: Vec<PatroniMember>,
}

/// `GET localhost:8008/cluster` inside `instance_id`'s container, parsed
/// into Patroni's member list. Returns `Err` on any failure (unreachable
/// container, non-JSON response, SSH/timeout failure) -- callers are
/// expected to degrade to "unknown" rather than propagate this as a hard
/// command failure.
pub async fn probe_cluster(instance_id: &str) -> Result<Vec<PatroniMember>> {
    let command = "curl -s --max-time 4 localhost:8008/cluster";
    // Retrying wrapper: a relay blip must not read as a dead member. The
    // timeout is per attempt (the wrapper owns the loop).
    let output = exec_probe_in_container(instance_id, command, PROBE_TIMEOUT)
        .await
        .context("Probing Patroni failed")?;

    let parsed: PatroniClusterResponse = serde_json::from_str(output.trim())
        .with_context(|| format!("Unexpected response from Patroni: {}", output.trim()))?;
    Ok(parsed.members)
}

/// Probes every reachable member and returns the first successful result.
/// Used when any single cluster member's `/cluster` view is representative
/// enough (Patroni's REST API returns the same cluster-wide member list from
/// any node) -- e.g. to resolve the current leader before a switchover.
///
/// On total failure, returns every member's own error instead of a bare
/// `None`: "could not reach Patroni" has historically meant anything from a
/// wedged cluster to the CALLER's SSH setup being unusable (the probes run
/// over `ssh <instance>@ssh.railway.com`), and only the underlying error
/// tells those apart.
pub async fn probe_any(
    instance_ids: &[String],
) -> Result<(String, Vec<PatroniMember>), Vec<(String, String)>> {
    let mut failures = Vec::with_capacity(instance_ids.len());
    for instance_id in instance_ids {
        match probe_cluster(instance_id).await {
            Ok(members) => return Ok((instance_id.clone(), members)),
            Err(e) => failures.push((instance_id.clone(), format!("{e:#}"))),
        }
    }
    Err(failures)
}

/// Resolves the member's Patroni REST credential from its OWN environment
/// and leaves `$@` holding curl's `-u user:pass` -- empty when the member
/// carries no password.
///
/// The `postgres-ha` image authenticates Patroni's MUTATING endpoints
/// (`POST /switchover`, `/failover`, `/restart`, `/reinitialize`,
/// `PATCH /config`) once the member carries a password, and the production
/// template sets `PATRONI_RESTAPI_PASSWORD` on every member -- so a bare
/// POST is answered `401 no auth header received` and the switchover never
/// happens. Reads (`GET /cluster` above) stay open, so they stay bare.
///
/// The precedence mirrors the image's own resolution (the dedicated REST
/// secret, else the superuser's), and the credential is read INSIDE the
/// container: nothing secret enters the exec payload, this process, or a
/// log line. A member with no password cannot be enforcing either, so the
/// call goes out bare and Patroni's answer is the truthful outcome -- one
/// command spans a fleet mid-rollout.
const RESTAPI_AUTH_PRELUDE: &str = concat!(
    r#"PATRONI_REST_PW="${PATRONI_RESTAPI_PASSWORD:-${PATRONI_SUPERUSER_PASSWORD:-${PGPASSWORD:-${POSTGRES_PASSWORD:-}}}}"; "#,
    r#"PATRONI_REST_USER="${PATRONI_RESTAPI_USERNAME:-${PATRONI_SUPERUSER_USERNAME:-${PGUSER:-${POSTGRES_USER:-postgres}}}}"; "#,
    r#"if [ -n "$PATRONI_REST_PW" ]; then set -- -u "$PATRONI_REST_USER:$PATRONI_REST_PW"; else set --; fi; "#,
);

/// The exact shell text the switchover runs, so a test can pin both halves:
/// the credential resolution and the request itself.
fn switchover_command(body: &str) -> String {
    format!(
        "{prelude}curl -s --max-time 8 -w '\\nHTTP_STATUS:%{{http_code}}' \"$@\" -X POST localhost:8008/switchover -H 'Content-Type: application/json' -d '{body}'",
        prelude = RESTAPI_AUTH_PRELUDE,
    )
}

/// `POST localhost:8008/switchover` against `instance_id`'s container,
/// asking Patroni to promote `candidate` off of `leader`. Patroni performs
/// the actual failover; this call just issues the request and surfaces a
/// non-2xx/timeout as an error.
pub async fn switchover(instance_id: &str, leader: &str, candidate: &str) -> Result<String> {
    let body = serde_json::json!({ "leader": leader, "candidate": candidate }).to_string();
    let command = switchover_command(&body);

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        exec_in_container(instance_id, &command),
    )
    .await
    .context("Timed out requesting switchover")??;

    parse_switchover_response(&output)
}

/// Splits the probe's `<body>\nHTTP_STATUS:<code>` (from `curl -w`) shape
/// and turns a non-2xx/unparseable status into an error carrying Patroni's
/// own response body (which explains WHY a switchover was rejected, e.g. a
/// candidate that isn't streaming).
fn parse_switchover_response(output: &str) -> Result<String> {
    let (response_body, status) = match output.rsplit_once("HTTP_STATUS:") {
        Some((body, status)) => (body.trim().to_string(), status.trim().parse::<u16>().ok()),
        None => (output.trim().to_string(), None),
    };

    match status {
        Some(200..=299) => Ok(response_body),
        Some(code) => bail!("Patroni switchover failed ({code}): {response_body}"),
        None => bail!("Patroni switchover returned an unexpected response: {response_body}"),
    }
}

/// Resolves each of `service_ids`' live **service instance** id (the
/// current deployment's instance, needed for `exec_in_container`) via one
/// shared `EnvironmentInstances` fetch. Service ids with no resolvable
/// instance (no active deployment) are simply omitted from the result --
/// callers degrade to "unknown" for those rather than failing outright.
pub async fn resolve_instance_ids(
    ctx: &ServiceContext,
    service_ids: &[String],
) -> Result<BTreeMap<String, String>> {
    let instances = get_environment_instances(
        &ctx.client,
        &ctx.configs,
        &ctx.project_id,
        &ctx.environment_id,
    )
    .await?;

    Ok(service_ids
        .iter()
        .filter_map(|id| {
            find_service_instance(&instances, id).map(|si| (id.clone(), si.id.clone()))
        })
        .collect())
}

/// One member's live probe result, keyed by Railway service id (not Patroni
/// member name) so callers can join it back against `HaState::members`.
#[derive(Debug, Clone, Default)]
pub struct MemberProbe {
    /// The member's own container was reachable and returned a parseable
    /// Patroni `/cluster` response (even if its own entry wasn't found in
    /// that response -- see `self_view`).
    pub reachable: bool,
    /// This member's own entry from its (or a fallback reachable member's)
    /// `/cluster` response, matched by lowercased service name.
    pub self_view: Option<PatroniMember>,
}

/// Probes every member's own container independently (so a network
/// partition that leaves one member unable to reach the rest is visible as
/// specifically THAT member being unreachable, not silently masked by a
/// healthy neighbor's response) and joins each result back to its own
/// entry, by lowercased service name, in whichever cluster response query
/// succeeded. Each probe already carries its own ~5s timeout
/// (`PROBE_TIMEOUT`); an unreachable/timed-out member degrades to
/// `MemberProbe::default()` (`reachable: false`) rather than failing the
/// whole probe.
pub async fn probe_members(
    ctx: &ServiceContext,
    members: &[(String, String)],
) -> Result<BTreeMap<String, MemberProbe>> {
    let service_ids: Vec<String> = members.iter().map(|(id, _)| id.clone()).collect();
    let instance_ids = resolve_instance_ids(ctx, &service_ids).await?;

    let probes = members.iter().map(|(service_id, service_name)| {
        let instance_id = instance_ids.get(service_id).cloned();
        let name_lower = service_name.to_ascii_lowercase();
        let service_id = service_id.clone();
        async move {
            let Some(instance_id) = instance_id else {
                return (service_id, MemberProbe::default());
            };
            match probe_cluster(&instance_id).await {
                Ok(cluster_members) => {
                    let self_view = cluster_members
                        .into_iter()
                        .find(|m| m.name.to_ascii_lowercase() == name_lower);
                    (
                        service_id,
                        MemberProbe {
                            reachable: true,
                            self_view,
                        },
                    )
                }
                Err(_) => (service_id, MemberProbe::default()),
            }
        }
    });

    Ok(futures::future::join_all(probes)
        .await
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the emitted switchover text through a real `sh`, with a `curl`
    /// shim on PATH that records the argv it was handed. Substring checks
    /// cannot catch a quoting slip; executing it can.
    fn curl_argv_for(env: &[(&str, &str)]) -> Vec<String> {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "cli-patroni-auth-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("curl");
        std::fs::write(&shim, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut child = std::process::Command::new("sh")
            .arg("-s")
            // The shim must WIN over a real curl, but `sh` itself still has to
            // be findable — replacing PATH outright makes the spawn fail.
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("PATRONI_RESTAPI_PASSWORD")
            .env_remove("PATRONI_SUPERUSER_PASSWORD")
            .env_remove("PGPASSWORD")
            .env_remove("POSTGRES_PASSWORD")
            .env_remove("PATRONI_RESTAPI_USERNAME")
            .env_remove("PATRONI_SUPERUSER_USERNAME")
            .env_remove("PGUSER")
            .env_remove("POSTGRES_USER")
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
            .write_all(
                switchover_command(r#"{"leader":"postgres-1","candidate":"postgres-2"}"#)
                    .as_bytes(),
            )
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "shell rejected the switchover text: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// An enforcing member gets HTTP Basic auth built from its own env, with
    /// the REST secret winning over the superuser's, and the request itself
    /// still intact behind it.
    #[test]
    fn switchover_authenticates_from_the_members_own_env() {
        let argv = curl_argv_for(&[
            ("PATRONI_RESTAPI_PASSWORD", "rest-pw"),
            ("PATRONI_SUPERUSER_PASSWORD", "super-pw"),
            ("POSTGRES_USER", "rw"),
        ]);
        let u = argv
            .iter()
            .position(|a| a == "-u")
            .expect("credential passed to curl");
        assert_eq!(argv[u + 1], "rw:rest-pw");
        assert!(argv.iter().any(|a| a == "localhost:8008/switchover"));
        assert!(
            argv.iter()
                .any(|a| a == r#"{"leader":"postgres-1","candidate":"postgres-2"}"#)
        );
        // The secret is resolved in the container, never in the payload.
        assert!(!switchover_command("{}").contains("rest-pw"));
    }

    /// The superuser's password is the fallback, and `postgres` the default
    /// username -- the same precedence the image resolves.
    #[test]
    fn switchover_falls_back_to_the_superuser_credential() {
        let argv = curl_argv_for(&[("PATRONI_SUPERUSER_PASSWORD", "super-pw")]);
        let u = argv
            .iter()
            .position(|a| a == "-u")
            .expect("credential passed to curl");
        assert_eq!(argv[u + 1], "postgres:super-pw");
    }

    /// A member with no password at all cannot be enforcing, so the POST
    /// goes out bare -- `-u ""` would turn a working cluster into a 401.
    #[test]
    fn switchover_stays_bare_when_the_member_has_no_password() {
        let argv = curl_argv_for(&[]);
        assert!(
            !argv.iter().any(|a| a == "-u"),
            "bare POST expected, got {argv:?}"
        );
        assert!(argv.iter().any(|a| a == "localhost:8008/switchover"));
    }

    #[test]
    fn parses_cluster_response_with_partial_fields() {
        let raw = r#"{"members": [
            {"name": "postgres-1", "role": "leader", "state": "running", "timeline": 3},
            {"name": "postgres-replica-1", "role": "replica", "state": "streaming", "lag": 0}
        ]}"#;
        let parsed: PatroniClusterResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.members.len(), 2);
        assert_eq!(parsed.members[0].role, "leader");
        assert_eq!(parsed.members[1].lag, Some(serde_json::json!(0)));
    }

    #[test]
    fn parses_cluster_response_tolerates_missing_fields() {
        let raw = r#"{"members": [{"name": "postgres-1"}]}"#;
        let parsed: PatroniClusterResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.members.len(), 1);
        assert_eq!(parsed.members[0].role, "");
        assert!(parsed.members[0].lag.is_none());
    }

    #[test]
    fn switchover_response_accepts_2xx_with_body() {
        let ok =
            parse_switchover_response("Successfully switched over to \"pg-2\"\nHTTP_STATUS:200")
                .unwrap();
        assert_eq!(ok, "Successfully switched over to \"pg-2\"");
    }

    #[test]
    fn switchover_response_surfaces_patronis_rejection_body() {
        let err = parse_switchover_response(
            "candidate name does not match with the switchover candidate\nHTTP_STATUS:412",
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("412"));
        assert!(message.contains("candidate name does not match"));
    }

    #[test]
    fn switchover_response_rejects_missing_status_marker() {
        let err = parse_switchover_response("curl: (7) connection refused").unwrap_err();
        assert!(err.to_string().contains("unexpected response"));
    }

    #[test]
    fn switchover_response_rejects_unparseable_status_code() {
        assert!(parse_switchover_response("body\nHTTP_STATUS:abc").is_err());
    }
}
