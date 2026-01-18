use std::{cmp::Ordering, collections::HashMap, fmt::Display};

use anyhow::Result;
use colored::Colorize;
use country_emoji::flag;
use serde_json::{Map, Value, json};

use crate::{
    client::post_graphql,
    config::Configs,
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
    let regions = post_graphql::<queries::Regions, _>(
        client,
        configs.get_backboard(),
        queries::regions::Variables,
    )
    .await?;
    Ok(regions)
}

/// Interactive prompt for selecting regions and replica counts.
/// Fetches regions from the API first, then prompts.
/// Returns a HashMap of region name -> replica count.
///
/// # Arguments
/// * `configs` - Railway configs for API access
/// * `client` - HTTP client
/// * `existing` - Current region config as JSON Value (region -> { numReplicas: n })
pub async fn prompt_for_regions(
    configs: &Configs,
    client: &reqwest::Client,
    existing: &Value,
) -> Result<HashMap<String, u64>> {
    let regions = fetch_regions(client, configs).await?;
    prompt_for_regions_with_data(regions, existing)
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
            .filter(|r| r.railway_metal.unwrap_or_default())
            .map(|f| {
                PromptRegion(
                    f.clone(),
                    format!(
                        "{} {}{}{}",
                        flag(&f.country).unwrap_or_default(),
                        f.location,
                        if f.railway_metal.unwrap_or_default() {
                            " (METAL)".bold().purple().to_string()
                        } else {
                            String::new()
                        },
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
                    region.0.name.clone()
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
