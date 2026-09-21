//! Ownership-only operations use the same Environment.configEtag as config
//! plans. They never evaluate authoring files or submit resource ChangeSets.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::client::post_graphql_raw;

use super::partial::{IacPartials, has_named_partials, parse_partial_name};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    pub name: String,
    pub config_etag: String,
    pub iac_partials: Option<IacPartials>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipResult {
    pub affected_resources: Vec<String>,
    pub iac_partials: IacPartials,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub environment_id: String,
    pub environment_name: String,
    pub base_config_etag: String,
    pub from_partial: String,
    pub to_partial: Option<String>,
    #[serde(flatten)]
    pub result: OwnershipResult,
}

pub fn named_partial(value: &str) -> Result<String, String> {
    parse_partial_name(Some(value))?.ok_or_else(|| "A named IaC partial is required.".to_string())
}

pub fn whole_project_available(owners: &IacPartials) -> bool {
    !has_named_partials(Some(owners))
}

pub async fn fetch_snapshot(
    client: &reqwest::Client,
    endpoint: &str,
    environment_id: &str,
) -> Result<Snapshot> {
    #[derive(Deserialize)]
    struct Response {
        environment: Snapshot,
    }
    // Do not use the config engine's compatibility fallback: unavailable
    // ownership data must never be interpreted as an empty ownership map.
    let data = post_graphql_raw::<Response, _>(
        client,
        endpoint,
        r#"query IacPartialOwnership($environmentId: String!) {
            environment(id: $environmentId) { id name configEtag iacPartials }
        }"#,
        json!({ "environmentId": environment_id }),
    )
    .await
    .context("Could not read IaC partial ownership")?;
    ensure!(
        !data.environment.config_etag.is_empty(),
        "The environment did not report a config etag. Refresh the ownership preview before proceeding."
    );
    Ok(data.environment)
}

pub fn preview(
    snapshot: Snapshot,
    from_partial: &str,
    to_partial: Option<&str>,
    resources: Option<&[String]>,
    base_config_etag: Option<&str>,
) -> Result<Preview> {
    let from_partial = named_partial(from_partial).map_err(anyhow::Error::msg)?;
    let to_partial = to_partial
        .map(named_partial)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    ensure!(
        to_partial.as_ref() != Some(&from_partial),
        "The source and destination partials must differ."
    );
    ensure!(
        !snapshot.config_etag.is_empty(),
        "The environment did not report a config etag."
    );
    if let Some(expected) = base_config_etag {
        ensure!(
            expected == snapshot.config_etag,
            "The environment changed since this ownership preview was computed. Re-run with --dry-run and review the affected resources."
        );
    }
    let mut owners = snapshot.iac_partials.unwrap_or_default();
    let mut affected_resources: Vec<String> = match resources {
        Some(resources) => {
            ensure!(
                !resources.is_empty(),
                "At least one resource address is required."
            );
            resources.to_vec()
        }
        None => owners
            .iter()
            .filter(|(_, owner)| **owner == from_partial)
            .map(|(address, _)| address.clone())
            .collect(),
    };
    affected_resources.sort();
    affected_resources.dedup();
    ensure!(
        !affected_resources.is_empty(),
        "No resources are owned by partial {from_partial:?}. Run `railway config partials list` to inspect ownership."
    );
    for address in &affected_resources {
        if owners.get(address) != Some(&from_partial) {
            bail!(
                "Resource {address:?} is not owned by partial {from_partial:?}. Run `railway config partials list` to inspect ownership."
            );
        }
    }
    for address in &affected_resources {
        if let Some(to) = &to_partial {
            owners.insert(address.clone(), to.clone());
        } else {
            owners.remove(address);
        }
    }
    Ok(Preview {
        environment_id: snapshot.id,
        environment_name: snapshot.name,
        base_config_etag: snapshot.config_etag,
        from_partial,
        to_partial,
        result: OwnershipResult {
            affected_resources,
            iac_partials: owners,
        },
    })
}

pub async fn apply(
    client: &reqwest::Client,
    endpoint: &str,
    preview: &Preview,
) -> Result<OwnershipResult> {
    // Always send the exact reviewed addresses, including for a whole partial.
    // The etag also protects configuration and ownership changes after review.
    let mut variables = json!({
        "environmentId": preview.environment_id,
        "resources": preview.result.affected_resources,
        "baseConfigEtag": preview.base_config_etag,
    });
    let mutation = if let Some(to) = &preview.to_partial {
        variables["fromPartial"] = json!(preview.from_partial);
        variables["toPartial"] = json!(to);
        r#"mutation IacPartialTransfer($environmentId: String!, $fromPartial: String!, $toPartial: String!, $resources: [String!], $baseConfigEtag: String) {
            result: environmentIacPartialTransfer(environmentId: $environmentId, fromPartial: $fromPartial, toPartial: $toPartial, resources: $resources, baseConfigEtag: $baseConfigEtag) { affectedResources iacPartials }
        }"#
    } else {
        variables["partial"] = json!(preview.from_partial);
        r#"mutation IacPartialRelease($environmentId: String!, $partial: String!, $resources: [String!], $baseConfigEtag: String) {
            result: environmentIacPartialRelease(environmentId: $environmentId, partial: $partial, resources: $resources, baseConfigEtag: $baseConfigEtag) { affectedResources iacPartials }
        }"#
    };
    #[derive(Deserialize)]
    struct Response {
        result: OwnershipResult,
    }
    let data = post_graphql_raw::<Response, _>(client, endpoint, mutation, variables)
        .await
        .context("Could not change IaC ownership. Re-run with --dry-run to refresh the preview before retrying")?;
    Ok(data.result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        serde_json::from_value(json!({
            "id": "env", "name": "production", "configEtag": "reviewed",
            "iacPartials": {
                "service.api": "legacy-ops",
                "service.admin": "legacy-ops",
                "bucket.media": "operations",
                "service.legacy": "*"
            }
        }))
        .unwrap()
    }

    #[test]
    fn transfer_uses_exact_sorted_addresses_and_preserves_other_owners() {
        let selected = vec![
            "service.api".into(),
            "service.admin".into(),
            "service.api".into(),
        ];
        for destination in ["operations", "new-partial"] {
            let preview = preview(
                snapshot(),
                "legacy-ops",
                Some(destination),
                Some(&selected),
                None,
            )
            .unwrap();
            assert_eq!(
                preview.result.affected_resources,
                ["service.admin", "service.api"]
            );
            assert_eq!(preview.base_config_etag, "reviewed");
            assert_eq!(preview.result.iac_partials["bucket.media"], "operations");
            assert_eq!(preview.result.iac_partials["service.legacy"], "*");
            assert_eq!(preview.result.iac_partials["service.api"], destination);
        }
    }

    #[test]
    fn partial_release_preserves_unselected_addresses() {
        let selected = vec!["service.api".into()];
        let preview = preview(
            snapshot(),
            "legacy-ops",
            None,
            Some(&selected),
            Some("reviewed"),
        )
        .unwrap();
        assert!(!preview.result.iac_partials.contains_key("service.api"));
        assert_eq!(preview.result.iac_partials["service.admin"], "legacy-ops");
        assert!(!whole_project_available(&preview.result.iac_partials));
    }

    #[test]
    fn whole_partial_release_only_enables_whole_project_after_final_named_owner() {
        let first = preview(snapshot(), "legacy-ops", None, None, None).unwrap();
        assert_eq!(first.result.affected_resources.len(), 2);
        assert!(!whole_project_available(&first.result.iac_partials));
        let mut next = snapshot();
        next.iac_partials = Some(first.result.iac_partials);
        let last = preview(next, "operations", None, None, None).unwrap();
        assert!(whole_project_available(&last.result.iac_partials));
        assert_eq!(last.result.iac_partials.len(), 1);
        assert_eq!(last.result.iac_partials["service.legacy"], "*");
    }

    #[test]
    fn selection_errors_do_not_expand_to_whole_partial() {
        for selected in [
            vec![],
            vec!["".into()],
            vec!["api".into()],
            vec!["service.missing".into()],
            vec!["service.api".into(), "bucket.media".into()],
        ] {
            assert!(preview(snapshot(), "legacy-ops", None, Some(&selected), None).is_err());
        }
        assert!(
            preview(snapshot(), "missing", None, None, None)
                .unwrap_err()
                .to_string()
                .contains("No resources")
        );
    }

    #[test]
    fn validates_names_and_rejects_same_destination() {
        for name in ["", " ", "*", "invalid/name", "two words", &"x".repeat(65)] {
            assert!(preview(snapshot(), name, None, None, None).is_err());
            assert!(preview(snapshot(), "legacy-ops", Some(name), None, None).is_err());
        }
        assert!(preview(snapshot(), "legacy-ops", Some("legacy-ops"), None, None).is_err());
        assert_eq!(named_partial(" Named._-123 ").unwrap(), "Named._-123");
        assert!(named_partial(&"x".repeat(64)).is_ok());
    }

    #[test]
    fn stale_or_missing_etag_cannot_be_previewed_for_execution() {
        assert!(
            preview(snapshot(), "legacy-ops", None, None, Some("older"))
                .unwrap_err()
                .to_string()
                .contains("changed since")
        );
        let mut missing = snapshot();
        missing.config_etag.clear();
        assert!(preview(missing, "legacy-ops", None, None, None).is_err());
    }

    #[test]
    fn empty_map_is_available_for_whole_project_but_cannot_release_missing_partial() {
        let mut empty = snapshot();
        empty.iac_partials = None;
        assert!(whole_project_available(&IacPartials::new()));
        assert!(preview(empty, "legacy-ops", None, None, None).is_err());
    }
}
