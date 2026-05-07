use crate::{
    commands::output::service_summary::print_scale_result,
    controllers::{
        environment::get_matched_environment,
        project::{ProjectEnvironmentInstances, find_service_instance, get_environment_instances},
        regions::{
            available_deploy_regions_help, build_multi_region_patch, convert_hashmap_to_map,
            fetch_region_locations_for_project, fetch_regions, fetch_regions_for_project,
            merge_config, prompt_for_regions_for_project, region_data_from_deployment_meta,
            region_flag_name, region_full_label, region_is_available, resolve_deploy_region_id,
        },
    },
    util::progress::create_spinner_if,
};
use anyhow::{Context as _, bail};
use clap::{Arg, Command, Parser};
use futures::executor::block_on;
use is_terminal::IsTerminal;
use serde_json::{Map, Value};
use std::collections::HashMap;
use struct_field_names_as_array::FieldNamesAsArray;

use super::*;

/// Dynamic flags workaround
/// Unfortunately, we aren't able to use the Parser derive macro when working with dynamic flags,
/// meaning we have to implement most of the traits for the Args struct manually.
struct DynamicArgs(HashMap<String, u64>);

#[derive(Parser, FieldNamesAsArray)]
#[clap(after_help = SCALE_AFTER_HELP)]
pub struct Args {
    #[clap(flatten)]
    dynamic: DynamicArgs,

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

Region names use the same friendly names as the Railway dashboard, formatted for the CLI.
Run `railway scale --help` while logged in to list available regions.

Backwards compatibility:
  Legacy region flags like `railway scale --eu-west 2` are still accepted."#;

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
        resolve_new_config(&args, &configs, &client, &linked_project.project, &existing).await?,
    );
    if new_config.is_empty() {
        if !args.json {
            println!("No changes made");
        }
        return Ok(());
    }
    let region_data = merge_config(existing, new_config);
    commit_scale_patch(
        &configs,
        &client,
        &environment_id,
        &service_id,
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
    existing: &Value,
) -> Result<HashMap<String, u64>> {
    if args.assignments.is_empty() && args.dynamic.0.is_empty() && std::io::stdout().is_terminal() {
        return prompt_for_regions_for_project(configs, client, Some(project_id), existing).await;
    }

    if args.assignments.is_empty() && args.dynamic.0.is_empty() {
        bail!(
            "Please specify replica counts as REGION=REPLICAS, for example `railway scale eu-west=2`"
        );
    }

    let mut new_config = args.dynamic.0.clone();
    if args.assignments.is_empty() {
        return Ok(new_config);
    }

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
        let region_id = resolve_deploy_region_id(&regions, region_input)?;

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
            commit_message: None,
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

/// This function generates flags that are appended to the command at runtime.
pub fn get_dynamic_args(cmd: Command) -> Command {
    if !is_top_level_scale() {
        return cmd;
    }
    add_region_args(cmd)
}

pub fn get_dynamic_args_for_service_subcommand(cmd: Command) -> Command {
    if !is_service_scale() {
        return cmd;
    }
    add_region_args(cmd)
}

fn is_top_level_scale() -> bool {
    let args: Vec<String> = std::env::args().collect();
    args.len() >= 2 && args[1].eq_ignore_ascii_case("scale")
}

fn is_service_scale() -> bool {
    let args: Vec<String> = std::env::args().collect();
    args.len() >= 3
        && args[1].eq_ignore_ascii_case("service")
        && args[2].eq_ignore_ascii_case("scale")
}

fn add_region_args(cmd: Command) -> Command {
    block_on(async move {
        let Ok(configs) = Configs::new() else {
            return cmd;
        };
        let Ok(client) = GQLClient::new_authorized(&configs) else {
            return cmd;
        };
        let Ok(regions) = fetch_regions(&client, &configs).await else {
            return cmd;
        };

        let available_regions = regions
            .regions
            .iter()
            .filter(|r| region_is_available(r))
            .collect::<Vec<_>>();
        let cmd = cmd.after_help(dynamic_scale_after_help(&regions.regions));

        available_regions.iter().fold(cmd, |new_cmd, region| {
            let region_id_static: &'static str = Box::leak(region.name.clone().into_boxed_str());
            let region_flag_static: &'static str =
                Box::leak(region_flag_name(region).into_boxed_str());
            let region_help = format!(
                "Number of instances to run in {}",
                region_full_label(region)
            );
            new_cmd.arg(
                Arg::new(region_id_static)
                    .long(region_flag_static)
                    .alias(region_id_static)
                    .help(region_help)
                    .hide(true)
                    .value_name("INSTANCES")
                    .value_parser(clap::value_parser!(u64))
                    .action(clap::ArgAction::Set),
            )
        })
    })
}

fn available_regions_help(regions: &[queries::regions::RegionsRegions]) -> String {
    available_deploy_regions_help(regions)
}

fn dynamic_scale_after_help(regions: &[queries::regions::RegionsRegions]) -> String {
    format!(
        "{SCALE_AFTER_HELP}\n\nAvailable regions:\n{}",
        available_regions_help(regions)
    )
}

impl clap::FromArgMatches for DynamicArgs {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let mut dynamic = HashMap::new();
        // Iterate through all provided argument keys.
        // Adjust the static key names if you add any to your Args struct.
        for key in matches.ids() {
            if Args::FIELD_NAMES_AS_ARRAY.contains(&key.as_str()) {
                continue;
            }
            // If the flag value can be interpreted as a u64, insert it.
            if let Some(val) = matches.get_one::<u64>(key.as_str()) {
                dynamic.insert(key.to_string(), *val);
            }
        }
        Ok(DynamicArgs(dynamic))
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl clap::Args for DynamicArgs {
    fn group_id() -> Option<clap::Id> {
        // Do not create an argument group for dynamic flags
        None
    }
    fn augment_args(cmd: clap::Command) -> clap::Command {
        // Leave the command unchanged; dynamic flags will be handled via FromArgMatches
        cmd
    }
    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        cmd
    }
}

impl clap::CommandFactory for DynamicArgs {
    fn command<'b>() -> clap::Command {
        let __clap_app = clap::Command::new("railwayapp");
        <DynamicArgs as clap::Args>::augment_args(__clap_app)
    }
    fn command_for_update<'b>() -> clap::Command {
        let __clap_app = clap::Command::new("railwayapp");
        <DynamicArgs as clap::Args>::augment_args_for_update(__clap_app)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
