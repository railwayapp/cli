use crate::{
    commands::output::service_summary::print_scale_result,
    controllers::{
        environment::get_matched_environment,
        project::{ProjectEnvironmentInstances, find_service_instance, get_environment_instances},
        regions::{
            build_multi_region_patch, convert_hashmap_to_map, fetch_region_locations_for_project,
            fetch_regions_for_project, merge_config, region_data_from_deployment_meta,
            resolve_deploy_region_id_for_scale, validate_total_replicas,
        },
        scale_tui::{self, ScaleTuiOutput},
    },
    util::progress::create_spinner_if,
};
use anyhow::{Context as _, bail};
use clap::{Command, Parser};
use is_terminal::IsTerminal;
use serde_json::{Map, Value};
use std::{collections::HashMap, ffi::OsString};

use super::*;

#[derive(Parser)]
#[clap(after_help = SCALE_AFTER_HELP)]
pub struct Args {
    /// Replica counts by region, e.g. eu-west=2 us-east=1
    #[clap(value_name = "REGION=REPLICAS")]
    assignments: Vec<String>,

    /// The service to scale (defaults to linked service)
    #[clap(long, short)]
    service: Option<String>,

    /// The environment the service is in (defaults to linked environment)
    #[clap(long, short)]
    environment: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

const SCALE_AFTER_HELP: &str = r#"Examples:

  railway scale eu-west=2
  railway scale --service worker eu-west=2 us-east=1
  railway service scale --service worker eu-west=2 us-east=1
  railway scale --environment production --service worker eu-west=0

Regions: us-west, us-east, eu-west, southeast-asia, or region IDs.
Maximum: 50 total replicas across regions."#;

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = post_graphql::<queries::Project, _>(
        &client,
        configs.get_backboard(),
        queries::project::Variables {
            id: linked_project.project.clone(),
        },
    )
    .await?
    .project;
    let environment = match args.environment.clone() {
        Some(env) => env,
        None => linked_project.environment_id()?.to_string(),
    };
    let environment = get_matched_environment(&project, environment)?;
    let environment_id = environment.id;
    let environment_name = environment.name;
    let environment_instances =
        get_environment_instances(&client, &configs, &linked_project.project, &environment_id)
            .await?;
    let (existing, service_id) =
        get_existing_config(&args, &linked_project, &project, &environment_instances)?;
    let service_name = project
        .services
        .edges
        .iter()
        .find(|service| service.node.id == service_id)
        .map(|service| service.node.name.clone())
        .expect("service ID returned from project services");
    let new_config = convert_hashmap_to_map(
        resolve_new_config(
            &args,
            &configs,
            &client,
            &linked_project.project,
            &service_name,
            &environment_name,
            &existing,
        )
        .await?,
    );
    if new_config.is_empty() {
        if !args.json {
            println!("No changes made");
        }
        return Ok(());
    }
    let region_data = merge_config(existing, new_config);
    validate_total_replicas(&region_data)?;
    commit_scale_patch(
        &configs,
        &client,
        &environment_id,
        &service_id,
        &service_name,
        &region_data,
        args.json,
    )
    .await?;

    if args.json {
        println!("{}", serde_json::json!({"regions": region_data}));
    } else {
        let region_locations =
            fetch_region_locations_for_project(&client, &configs, Some(&linked_project.project))
                .await;
        print_scale_result(
            &service_name,
            &service_id,
            &environment_name,
            &region_data,
            &region_locations,
        );
    }

    Ok(())
}

async fn resolve_new_config(
    args: &Args,
    configs: &Configs,
    client: &reqwest::Client,
    project_id: &str,
    service_name: &str,
    environment_name: &str,
    existing: &Value,
) -> Result<HashMap<String, u64>> {
    if args.assignments.is_empty() && std::io::stdout().is_terminal() {
        let regions = fetch_regions_for_project(client, configs, Some(project_id)).await?;
        return match scale_tui::run(scale_tui::ScaleTuiParams {
            service_name: service_name.to_string(),
            environment_name: environment_name.to_string(),
            regions,
            existing: existing.clone(),
        })? {
            ScaleTuiOutput::Apply(changes) => Ok(changes),
            ScaleTuiOutput::Cancelled => Ok(HashMap::new()),
        };
    }

    if args.assignments.is_empty() {
        bail!(
            "Please specify replica counts as REGION=REPLICAS, for example `railway scale eu-west=2`"
        );
    }

    let mut new_config = HashMap::new();
    let regions = fetch_regions_for_project(client, configs, Some(project_id)).await?;

    for assignment in &args.assignments {
        let (region_input, replicas_input) = assignment.split_once('=').with_context(|| {
            format!(
                "Invalid scale target `{assignment}`. Use REGION=REPLICAS, for example `eu-west=2`"
            )
        })?;
        if region_input.trim().is_empty() {
            bail!("Invalid scale target `{assignment}`. Region cannot be empty");
        }

        let replicas = replicas_input.parse::<u64>().with_context(|| {
            format!("Invalid replica count `{replicas_input}` in `{assignment}`")
        })?;
        let region_id =
            resolve_deploy_region_id_for_scale(&regions, region_input, replicas, existing)?;

        if new_config.insert(region_id.clone(), replicas).is_some() {
            bail!("Region `{}` was specified more than once", region_input);
        }
    }

    Ok(new_config)
}

async fn commit_scale_patch(
    configs: &Configs,
    client: &reqwest::Client,
    environment_id: &str,
    service_id: &str,
    service_name: &str,
    region_data: &Value,
    json: bool,
) -> Result<(), anyhow::Error> {
    let spinner = create_spinner_if(!json, "Committing scale changes...".into());
    let patch = build_multi_region_patch(service_id, region_data)?;

    post_graphql::<mutations::EnvironmentPatchCommit, _>(
        client,
        configs.get_backboard(),
        mutations::environment_patch_commit::Variables {
            environment_id: environment_id.to_string(),
            patch,
            commit_message: Some(format!("Scale service {service_name}")),
        },
    )
    .await?;
    if let Some(s) = &spinner {
        s.finish_with_message("Scale change committed");
    }
    Ok(())
}

/// Returns (existing_config, service_id)
fn get_existing_config(
    args: &Args,
    linked_project: &LinkedProject,
    project: &queries::project::ProjectProject,
    environment_instances: &ProjectEnvironmentInstances,
) -> Result<(Value, String)> {
    let service_input = match args.service.as_ref() {
        Some(s) => s,
        None => linked_project.service.as_ref().ok_or_else(|| {
            anyhow::anyhow!("No service linked. Please either specify a service with the --service flag or link one with `railway service`")
        })?,
    };

    let service = project.services.edges.iter().find(|p| {
        (p.node.id == *service_input)
            || (p.node.name.to_lowercase() == service_input.to_lowercase())
    });

    let Some(service) = service else {
        bail!("Service '{}' not found in project", service_input);
    };

    let service_id = service.node.id.clone();

    // check that service exists in that environment
    let instance = find_service_instance(environment_instances, &service_id);
    let service_meta = if let Some(instance) = instance {
        if let Some(latest) = &instance.latest_deployment {
            if let Some(meta) = &latest.meta {
                region_data_from_deployment_meta(meta)?
            } else {
                None
            }
        } else {
            None
        }
    } else {
        bail!("Service not found in the environment")
    };

    Ok((
        service_meta.unwrap_or(Value::Object(Map::new())),
        service_id,
    ))
}

/// Legacy region flags are normalized before clap parses argv, so scale no
/// longer performs any network-backed dynamic command construction here.
pub fn get_dynamic_args(cmd: Command) -> Command {
    cmd
}

pub fn get_dynamic_args_for_service_subcommand(cmd: Command) -> Command {
    cmd
}

pub fn normalize_legacy_scale_args(args: Vec<OsString>) -> Vec<OsString> {
    let Some(scale_args_start) = scale_args_start(&args) else {
        return args;
    };

    let mut normalized = args[..scale_args_start].to_vec();
    normalize_scale_args_tail(&args[scale_args_start..], &mut normalized);
    normalized
}

fn scale_args_start(args: &[OsString]) -> Option<usize> {
    if args.get(1).is_some_and(|arg| os_eq(arg, "scale")) {
        Some(2)
    } else if args.get(1).is_some_and(|arg| os_eq(arg, "service"))
        && args.get(2).is_some_and(|arg| os_eq(arg, "scale"))
    {
        Some(3)
    } else {
        None
    }
}

fn normalize_scale_args_tail(args: &[OsString], normalized: &mut Vec<OsString>) {
    let mut idx = 0;
    while idx < args.len() {
        let current = &args[idx];
        let Some(current_str) = current.to_str() else {
            normalized.push(current.clone());
            idx += 1;
            continue;
        };

        if current_str == "--" {
            normalized.extend(args[idx..].iter().cloned());
            break;
        }

        if let Some(flag) = current_str.strip_prefix("--") {
            let (flag_name, inline_value) = flag
                .split_once('=')
                .map_or((flag, None), |(name, value)| (name, Some(value)));

            if scale_long_flag_takes_value(flag_name) {
                normalized.push(current.clone());
                idx += 1;
                if inline_value.is_none() && idx < args.len() {
                    normalized.push(args[idx].clone());
                    idx += 1;
                }
                continue;
            }

            if scale_long_flag_is_known(flag_name) {
                normalized.push(current.clone());
                idx += 1;
                continue;
            }

            if let Some(value) = inline_value {
                normalized.push(OsString::from(format!("{flag_name}={value}")));
                idx += 1;
                continue;
            }

            if let Some(next) = args.get(idx + 1).and_then(|value| value.to_str())
                && !next.starts_with('-')
            {
                normalized.push(OsString::from(format!("{flag_name}={next}")));
                idx += 2;
                continue;
            }
        }

        if matches!(current_str, "-s" | "-e") {
            normalized.push(current.clone());
            idx += 1;
            if idx < args.len() {
                normalized.push(args[idx].clone());
                idx += 1;
            }
            continue;
        }

        normalized.push(current.clone());
        idx += 1;
    }
}

fn scale_long_flag_takes_value(flag: &str) -> bool {
    matches!(flag, "service" | "environment")
}

fn scale_long_flag_is_known(flag: &str) -> bool {
    matches!(flag, "json" | "help" | "version") || scale_long_flag_takes_value(flag)
}

fn os_eq(value: &OsString, expected: &str) -> bool {
    value
        .to_str()
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn normalize(args: &[&str]) -> Vec<String> {
        normalize_legacy_scale_args(args.iter().map(OsString::from).collect())
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn legacy_region_flags_normalize_to_assignments() {
        assert_eq!(
            normalize(&["railway", "scale", "--eu-west", "2"]),
            vec!["railway", "scale", "eu-west=2"]
        );
        assert_eq!(
            normalize(&["railway", "scale", "--eu-west=2"]),
            vec!["railway", "scale", "eu-west=2"]
        );
    }

    #[test]
    fn legacy_region_flags_normalize_for_service_scale() {
        assert_eq!(
            normalize(&["railway", "service", "scale", "--us-east", "1"]),
            vec!["railway", "service", "scale", "us-east=1"]
        );
    }

    #[test]
    fn known_scale_flags_are_preserved() {
        assert_eq!(
            normalize(&[
                "railway",
                "scale",
                "--service",
                "worker",
                "--environment=production",
                "--json",
                "eu-west=2",
            ]),
            vec![
                "railway",
                "scale",
                "--service",
                "worker",
                "--environment=production",
                "--json",
                "eu-west=2",
            ]
        );
    }

    #[test]
    fn legacy_normalization_stops_after_arg_terminator() {
        assert_eq!(
            normalize(&["railway", "scale", "--", "--eu-west", "2"]),
            vec!["railway", "scale", "--", "--eu-west", "2"]
        );
    }

    #[test]
    fn build_scale_patch_targets_service_multi_region_config() {
        let patch = build_multi_region_patch(
            "svc_123",
            &json!({
                "europe-west4-drams3a": { "numReplicas": 2 },
                "us-east4-eqdc4a": null
            }),
        )
        .unwrap();

        let service = patch.services.get("svc_123").unwrap();
        let multi_region_config = service
            .deploy
            .as_ref()
            .unwrap()
            .multi_region_config
            .as_ref()
            .unwrap();

        assert_eq!(
            multi_region_config["europe-west4-drams3a"]
                .as_ref()
                .unwrap()
                .num_replicas,
            Some(2)
        );
        assert!(multi_region_config["us-east4-eqdc4a"].is_none());

        assert_eq!(
            serde_json::to_value(patch).unwrap(),
            json!({
                "services": {
                    "svc_123": {
                        "deploy": {
                            "multiRegionConfig": {
                                "europe-west4-drams3a": { "numReplicas": 2 },
                                "us-east4-eqdc4a": null
                            }
                        }
                    }
                }
            })
        );
    }
}
