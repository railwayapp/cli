use std::time::Duration;

use super::PatchEntry;
use crate::{
    GQLClient,
    config::Configs,
    consts::TICK_STRING,
    controllers::{
        config::environment::ServiceInstance,
        regions::{convert_hashmap_to_map, fetch_regions, prompt_for_regions_with_data},
    },
};
use anyhow::Result;
use futures::executor::block_on;
use serde_json::Value;

pub fn parse_interactive(
    service_id: &str,
    _service_name: &str,
    existing: Option<&ServiceInstance>,
) -> Result<Vec<PatchEntry>> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    // Show spinner while loading regions
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message("Loading regions...");
    spinner.enable_steady_tick(Duration::from_millis(100));

    // Fetch regions (async, with spinner)
    let regions = block_on(fetch_regions(&client, &configs))?;

    // Clear spinner before prompting
    spinner.finish_and_clear();

    // Extract existing multi-region config if available, otherwise use empty
    let existing_config = existing
        .and_then(|e| e.deploy.as_ref())
        .and_then(|d| d.multi_region_config.as_ref())
        .map(|config| serde_json::to_value(config).unwrap_or(Value::Object(serde_json::Map::new())))
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    // Prompt for regions (sync, no spinner)
    let updated = prompt_for_regions_with_data(regions, &existing_config)?;

    if updated.is_empty() {
        return Ok(vec![]);
    }

    let region_map = convert_hashmap_to_map(updated);

    // Convert to patch entries
    let mut entries: Vec<PatchEntry> = Vec::new();
    let base_path = format!("services.{service_id}.deploy.multiRegionConfig");

    for (region, config) in region_map {
        entries.push((format!("{base_path}.{region}"), config));
    }

    Ok(entries)
}
