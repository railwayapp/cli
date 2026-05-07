use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
    fmt::Display,
};

use anyhow::{Context as _, Result, bail};
use colored::Colorize;
use country_emoji::flag;
use json_dotpath::DotPaths as _;
use serde_json::{Map, Value, json};

use crate::{
    client::post_graphql,
    config::Configs,
    controllers::config::{DeployConfig, EnvironmentConfig, RegionConfig, ServiceInstance},
    gql::queries,
    util::prompt::{
        prompt_select_with_cancel, prompt_u64_with_placeholder_and_validation_and_cancel,
    },
};

/// Wrapper for region display in prompts
pub struct PromptRegion(pub queries::regions::RegionsRegions, pub String);

impl Display for PromptRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.1)
    }
}

/// Fetch available regions from the API
pub async fn fetch_regions(
    client: &reqwest::Client,
    configs: &Configs,
) -> Result<queries::regions::ResponseData> {
    fetch_regions_for_project(client, configs, None).await
}

pub async fn fetch_regions_for_project(
    client: &reqwest::Client,
    configs: &Configs,
    project_id: Option<&str>,
) -> Result<queries::regions::ResponseData> {
    let regions = post_graphql::<queries::Regions, _>(
        client,
        configs.get_backboard(),
        queries::regions::Variables {
            project_id: project_id.map(ToString::to_string),
        },
    )
    .await?;
    Ok(regions)
}

pub async fn fetch_region_locations(
    client: &reqwest::Client,
    configs: &Configs,
) -> HashMap<String, String> {
    fetch_region_locations_for_project(client, configs, None).await
}

pub async fn fetch_region_locations_for_project(
    client: &reqwest::Client,
    configs: &Configs,
    project_id: Option<&str>,
) -> HashMap<String, String> {
    match fetch_regions_for_project(client, configs, project_id).await {
        Ok(regions) => region_locations_from_regions(&regions.regions),
        Err(_) => HashMap::new(),
    }
}

pub fn region_locations_from_regions(
    regions: &[queries::regions::RegionsRegions],
) -> HashMap<String, String> {
    regions
        .iter()
        .filter(|r| !r.location.is_empty())
        .flat_map(|r| {
            let mut entries = vec![(r.name.clone(), r.location.clone())];
            if let Some(region) = &r.region {
                entries.push((region.clone(), r.location.clone()));
            }
            entries
        })
        .collect()
}

pub fn region_is_available(region: &queries::regions::RegionsRegions) -> bool {
    !region
        .deployment_constraints
        .as_ref()
        .and_then(|constraints| constraints.deprecation_info.as_ref())
        .is_some_and(|deprecation_info| deprecation_info.is_deprecated)
}

pub fn region_friendly_name(region: &queries::regions::RegionsRegions) -> &str {
    if region.location.is_empty() {
        &region.name
    } else {
        &region.location
    }
}

pub fn region_full_label(region: &queries::regions::RegionsRegions) -> String {
    if let Some(provider_region) = &region.region {
        format!(
            "{} ({}, {})",
            region_friendly_name(region),
            provider_region,
            region.country
        )
    } else {
        format!("{} ({})", region_friendly_name(region), region.country)
    }
}

pub fn region_flag_name(region: &queries::regions::RegionsRegions) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;

    for ch in region_friendly_name(region).chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator && !slug.is_empty() {
            slug.push('-');
            last_was_separator = true;
        }
    }

    slug.trim_end_matches('-').to_string()
}

pub fn region_matches_input(region: &queries::regions::RegionsRegions, input: &str) -> bool {
    region.name.eq_ignore_ascii_case(input)
        || region_flag_name(region) == input.to_ascii_lowercase()
        || region_friendly_name(region).eq_ignore_ascii_case(input)
}

pub fn resolve_deploy_region_id(
    regions: &queries::regions::ResponseData,
    input: &str,
) -> Result<String> {
    let matches = regions
        .regions
        .iter()
        .filter(|region| region_is_available(region) && region_matches_input(region, input))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [region] => Ok(region.name.clone()),
        [] => bail!(
            "Unknown region `{}`. Available regions:\n{}",
            input,
            available_deploy_regions_help(&regions.regions)
        ),
        regions => bail!(
            "Region `{}` is ambiguous. Matching regions:\n{}",
            input,
            regions
                .iter()
                .map(|region| format!(
                    "  {:<16} {}",
                    region_flag_name(region),
                    region_full_label(region)
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

pub fn available_deploy_regions_help(regions: &[queries::regions::RegionsRegions]) -> String {
    regions
        .iter()
        .filter(|region| region_is_available(region))
        .map(|region| {
            format!(
                "  {:<16} {}",
                region_flag_name(region),
                region_full_label(region)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn region_display_name(region: &str, region_locations: &HashMap<String, String>) -> String {
    region_locations
        .get(region)
        .cloned()
        .or_else(|| friendly_region_fallback(region))
        .unwrap_or_else(|| region.to_string())
}

pub fn format_region_replicas(
    region_data: &Value,
    region_locations: &HashMap<String, String>,
) -> String {
    let mut regions = region_data
        .as_object()
        .map(|config| {
            config
                .iter()
                .map(|(name, value)| {
                    let replicas = value
                        .get("numReplicas")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    (name.clone(), replicas)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    regions.sort_by(|a, b| a.0.cmp(&b.0));

    if regions.len() == 1 {
        return format!(
            "{} ({})",
            region_display_name(&regions[0].0, region_locations),
            regions[0].1
        );
    }

    let sep = " · ".dimmed();
    regions
        .iter()
        .map(|(name, replicas)| {
            format!(
                "{} ({})",
                region_display_name(name, region_locations),
                replicas.to_string().dimmed()
            )
        })
        .collect::<Vec<_>>()
        .join(&sep.to_string())
}

fn friendly_region_fallback(region: &str) -> Option<String> {
    let normalized = region.to_ascii_lowercase();
    let label = if normalized.starts_with("europe-west") {
        "EU West"
    } else if normalized.starts_with("europe-north") {
        "EU North"
    } else if normalized.starts_with("europe-south") {
        "EU South"
    } else if normalized.starts_with("europe-central") {
        "EU Central"
    } else if normalized.starts_with("us-west") || normalized.starts_with("northamerica-west") {
        "US West"
    } else if normalized.starts_with("us-east") || normalized.starts_with("northamerica-east") {
        "US East"
    } else if normalized.starts_with("us-central") || normalized.starts_with("northamerica-central")
    {
        "US Central"
    } else if normalized.starts_with("asia-east") {
        "Asia East"
    } else if normalized.starts_with("asia-southeast") {
        "Asia Southeast"
    } else if normalized.starts_with("asia-south") {
        "Asia South"
    } else if normalized.starts_with("australia") {
        "Australia"
    } else if normalized.starts_with("southamerica") {
        "South America"
    } else {
        return None;
    };

    Some(label.to_string())
}

/// Interactive prompt for selecting regions and replica counts.
/// Uses pre-fetched region data.
/// Returns a HashMap of region name -> replica count.
///
/// # Arguments
/// * `regions` - Pre-fetched region data
/// * `existing` - Current region config as JSON Value (region -> { numReplicas: n })
pub fn prompt_for_regions_with_data(
    mut regions: queries::regions::ResponseData,
    existing: &Value,
) -> Result<HashMap<String, u64>> {
    let mut updated: HashMap<String, u64> = HashMap::new();

    loop {
        let get_replicas_amount = |name: String| {
            let before = if let Some(num) = existing.get(name.clone()) {
                num.get("numReplicas").unwrap().as_u64().unwrap()
            } else {
                0
            };
            let after = if let Some(new_value) = updated.get(&name) {
                *new_value
            } else {
                before
            };
            (before, after)
        };

        regions.regions.sort_by(|a, b| {
            get_replicas_amount(b.name.clone())
                .1
                .cmp(&get_replicas_amount(a.name.clone()).1)
        });

        let region_options = regions
            .regions
            .iter()
            .filter(|r| region_is_available(r))
            .map(|f| {
                PromptRegion(
                    f.clone(),
                    format!(
                        "{} {}{}",
                        flag(&f.country).unwrap_or_default(),
                        f.location,
                        {
                            let (before, after) = get_replicas_amount(f.name.clone());
                            let amount = format!(
                                " ({} replica{})",
                                after,
                                if after == 1 { "" } else { "s" }
                            );
                            match after.cmp(&before) {
                                Ordering::Equal if after == 0 => String::new().normal(),
                                Ordering::Equal => amount.yellow(),
                                Ordering::Greater => amount.green(),
                                Ordering::Less => amount.red(),
                            }
                            .to_string()
                        }
                    ),
                )
            })
            .collect::<Vec<PromptRegion>>();

        let selected =
            prompt_select_with_cancel("Select a region <esc to finish>", region_options)?;

        if let Some(region) = selected {
            let amount_before = if let Some(updated) = updated.get(&region.0.name) {
                *updated
            } else if let Some(previous) = existing.as_object().unwrap().get(&region.0.name) {
                previous.get("numReplicas").unwrap().as_u64().unwrap()
            } else {
                0
            };

            let prompted = prompt_u64_with_placeholder_and_validation_and_cancel(
                format!(
                    "Enter the amount of replicas for {} <esc to go back>",
                    region_friendly_name(&region.0)
                )
                .as_str(),
                amount_before.to_string().as_str(),
            )?;

            if let Some(prompted) = prompted {
                let parse: u64 = prompted.parse()?;
                updated.insert(region.0.name.clone(), parse);
            }
            // If esc pressed when entering number, continue loop to select another region
        } else {
            // They pressed esc to finish
            break;
        }
    }

    Ok(updated)
}

/// Convert a HashMap of region -> replicas into a serde_json Map
/// with the format expected by the API: { region: { numReplicas: n } | null }
pub fn convert_hashmap_to_map(map: HashMap<String, u64>) -> Map<String, Value> {
    map.iter().fold(Map::new(), |mut m, (key, val)| {
        m.insert(
            key.clone(),
            if *val == 0 {
                Value::Null // this is how the dashboard does it
            } else {
                json!({ "numReplicas": val })
            },
        );
        m
    })
}

/// Merge existing config with new config
pub fn merge_config(existing: Value, new_config: Map<String, Value>) -> Value {
    let mut map = match existing {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    map.extend(new_config);
    Value::Object(map)
}

pub fn region_data_from_deploy(deploy: &Value) -> Result<Option<Value>> {
    if let Some(c) = deploy.dot_get::<Value>("multiRegionConfig")? {
        return Ok(Some(c));
    }

    if let Some(region) = deploy.dot_get::<String>("region")? {
        // Old deployments only have numReplicas and a region field.
        let mut map = Map::new();
        let replicas = deploy.dot_get::<Value>("numReplicas")?.unwrap_or(json!(1));
        map.insert(region, json!({ "numReplicas": replicas }));
        return Ok(Some(Value::Object(map)));
    }

    Ok(None)
}

pub fn region_data_from_deployment_meta(meta: &Value) -> Result<Option<Value>> {
    let Some(deploy) = meta.dot_get::<Value>("serviceManifest.deploy")? else {
        return Ok(None);
    };

    region_data_from_deploy(&deploy)
}

pub fn build_multi_region_patch(
    service_id: &str,
    region_data: &Value,
) -> Result<EnvironmentConfig> {
    let multi_region_config: BTreeMap<String, Option<RegionConfig>> =
        serde_json::from_value(region_data.clone())
            .context("Failed to build environment patch for region config")?;

    let mut services = BTreeMap::new();
    services.insert(
        service_id.to_string(),
        ServiceInstance {
            deploy: Some(DeployConfig {
                multi_region_config: Some(multi_region_config),
                ..DeployConfig::default()
            }),
            ..ServiceInstance::default()
        },
    );

    Ok(EnvironmentConfig {
        services,
        ..EnvironmentConfig::default()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketRegion {
    Sjc,
    Iad,
    Ams,
    Sin,
}

impl BucketRegion {
    pub fn code(self) -> &'static str {
        match self {
            Self::Sjc => "sjc",
            Self::Iad => "iad",
            Self::Ams => "ams",
            Self::Sin => "sin",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sjc => "US West, California",
            Self::Iad => "US East, Virginia",
            Self::Ams => "EU West, Amsterdam",
            Self::Sin => "Asia Pacific, Singapore",
        }
    }

    pub fn country(self) -> &'static str {
        match self {
            Self::Sjc | Self::Iad => "US",
            Self::Ams => "NL",
            Self::Sin => "SG",
        }
    }

    pub fn parse(input: &str) -> Result<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "sjc" => Ok(Self::Sjc),
            "iad" => Ok(Self::Iad),
            "ams" => Ok(Self::Ams),
            "sin" => Ok(Self::Sin),
            _ => bail!("Invalid bucket region \"{input}\". Valid regions: sjc, iad, ams, sin."),
        }
    }

    pub fn all() -> Vec<Self> {
        vec![Self::Sjc, Self::Iad, Self::Ams, Self::Sin]
    }

    pub fn display_for_code(code: &str) -> String {
        Self::parse(code)
            .map(|region| region.to_string())
            .unwrap_or_else(|_| code.to_string())
    }
}

impl Display for BucketRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.code(), self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(is_deprecated: Option<bool>) -> queries::regions::RegionsRegions {
        queries::regions::RegionsRegions {
            name: "us-west2".to_string(),
            region: Some("us-west2".to_string()),
            country: "US".to_string(),
            location: "US West".to_string(),
            workspace_id: None,
            deployment_constraints: is_deprecated.map(|is_deprecated| {
                queries::regions::RegionsRegionsDeploymentConstraints {
                    deprecation_info: Some(
                        queries::regions::RegionsRegionsDeploymentConstraintsDeprecationInfo {
                            is_deprecated,
                            replacement_region: "us-west2".to_string(),
                        },
                    ),
                }
            }),
        }
    }

    #[test]
    fn deprecated_regions_are_not_available() {
        assert!(!region_is_available(&region(Some(true))));
    }

    #[test]
    fn region_matching_accepts_friendly_slug_and_region_id() {
        let region = region(None);
        assert!(region_matches_input(&region, "us-west"));
        assert!(region_matches_input(&region, "us-west2"));
        assert!(region_matches_input(&region, "US West"));
    }

    #[test]
    fn legacy_deploy_region_config_returns_region_map() {
        let deploy = json!({
            "region": "us-west2",
            "numReplicas": 3
        });

        assert_eq!(
            region_data_from_deploy(&deploy).unwrap(),
            Some(json!({
                "us-west2": { "numReplicas": 3 }
            }))
        );
    }

    #[test]
    fn deployment_meta_region_config_returns_multi_region_map() {
        let meta = json!({
            "serviceManifest": {
                "deploy": {
                    "multiRegionConfig": {
                        "europe-west4-drams3a": { "numReplicas": 2 },
                        "us-east4-eqdc4a": { "numReplicas": 1 }
                    }
                }
            }
        });

        assert_eq!(
            region_data_from_deployment_meta(&meta).unwrap(),
            Some(json!({
                "europe-west4-drams3a": { "numReplicas": 2 },
                "us-east4-eqdc4a": { "numReplicas": 1 }
            }))
        );
    }
}
