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

use super::exec::exec_in_container;
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
    let output = tokio::time::timeout(PROBE_TIMEOUT, exec_in_container(instance_id, command))
        .await
        .context("Timed out probing Patroni")??;

    let parsed: PatroniClusterResponse = serde_json::from_str(output.trim())
        .with_context(|| format!("Unexpected response from Patroni: {}", output.trim()))?;
    Ok(parsed.members)
}

/// Probes every reachable member and returns the first successful result.
/// Used when any single cluster member's `/cluster` view is representative
/// enough (Patroni's REST API returns the same cluster-wide member list from
/// any node) -- e.g. to resolve the current leader before a switchover.
pub async fn probe_any(instance_ids: &[String]) -> Option<(String, Vec<PatroniMember>)> {
    for instance_id in instance_ids {
        if let Ok(members) = probe_cluster(instance_id).await {
            return Some((instance_id.clone(), members));
        }
    }
    None
}

/// `POST localhost:8008/switchover` against `instance_id`'s container,
/// asking Patroni to promote `candidate` off of `leader`. Patroni performs
/// the actual failover; this call just issues the request and surfaces a
/// non-2xx/timeout as an error.
pub async fn switchover(instance_id: &str, leader: &str, candidate: &str) -> Result<String> {
    let body = serde_json::json!({ "leader": leader, "candidate": candidate }).to_string();
    let command = format!(
        "curl -s --max-time 8 -w '\\nHTTP_STATUS:%{{http_code}}' -X POST localhost:8008/switchover -H 'Content-Type: application/json' -d '{body}'"
    );

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        exec_in_container(instance_id, &command),
    )
    .await
    .context("Timed out requesting switchover")??;

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
}
