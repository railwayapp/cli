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
//! container reaches them with no port-forwarding.

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

/// Asks `instance_id`'s own colocated coordinator to make THAT node the
/// primary. A 2xx means the handoff was accepted -- never that it completed;
/// confirmation comes from the role endpoint flipping, which is the same
/// signal everything else reads. Anything else is the coordinator's own
/// refusal, surfaced with its body as the reason.
pub async fn request_switchover(instance_id: &str, endpoint: &ResolvedEndpoint) -> Result<String> {
    let command = format!(
        "curl -s --max-time 8 -w '\\nHTTP_STATUS:%{{http_code}}' -X POST localhost:{}{}",
        endpoint.port, endpoint.path
    );

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
}
