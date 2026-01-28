use crate::{
    controllers::{
        environment::get_matched_environment,
        project::find_service_instance,
        regions::{convert_hashmap_to_map, merge_config, prompt_for_regions},
    },
    util::progress::create_spinner_if,
};
use anyhow::bail;
use clap::{Arg, Command, Parser};
use futures::executor::block_on;
use is_terminal::IsTerminal;
use json_dotpath::DotPaths as _;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use struct_field_names_as_array::FieldNamesAsArray;

use super::*;

/// Dynamic flags workaround
/// Unfortunately, we aren't able to use the Parser derive macro when working with dynamic flags,
/// meaning we have to implement most of the traits for the Args struct manually.
struct DynamicArgs(HashMap<String, u64>);

#[derive(Parser, FieldNamesAsArray)]
pub struct Args {
    #[clap(flatten)]
    dynamic: DynamicArgs,

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
    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());
    let (existing, service_id) =
        get_existing_config(&args, &linked_project, &project, &environment)?;
    let new_config = convert_hashmap_to_map(
        if args.dynamic.0.is_empty() && std::io::stdout().is_terminal() {
            prompt_for_regions(&configs, &client, &existing).await?
        } else if args.dynamic.0.is_empty() {
            bail!("Please specify regions via the flags when not running in a terminal")
        } else {
            args.dynamic.0
        },
    );
    if new_config.is_empty() {
        if !args.json {
            println!("No changes made");
        }
        return Ok(());
    }
    let region_data = merge_config(existing, new_config);
    let environment_id = get_matched_environment(&project, environment)?.id;
    update_regions_and_redeploy(
        configs,
        client,
        &environment_id,
        &service_id,
        region_data.clone(),
        args.json,
    )
    .await?;

    if args.json {
        println!("{}", serde_json::json!({"regions": region_data}));
    }

    Ok(())
}

async fn update_regions_and_redeploy(
    configs: Configs,
    client: reqwest::Client,
    environment_id: &str,
    service_id: &str,
    region_data: Value,
    json: bool,
) -> Result<(), anyhow::Error> {
    let spinner = create_spinner_if(!json, "Updating regions...".into());
    post_graphql::<mutations::UpdateRegions, _>(
        &client,
        configs.get_backboard(),
        mutations::update_regions::Variables {
            environment_id: environment_id.to_string(),
            service_id: service_id.to_string(),
            multi_region_config: region_data,
        },
    )
    .await?;
    if let Some(s) = &spinner {
        s.finish_with_message("Regions updated");
    }
    let spinner2 = create_spinner_if(!json, "Redeploying...".into());
    post_graphql::<mutations::ServiceInstanceDeploy, _>(
        &client,
        configs.get_backboard(),
        mutations::service_instance_deploy::Variables {
            environment_id: environment_id.to_string(),
            service_id: service_id.to_string(),
        },
    )
    .await?;
    if let Some(s) = spinner2 {
        s.finish_with_message("Redeployed");
    }
    Ok(())
}

/// Returns (existing_config, service_id)
fn get_existing_config(
    args: &Args,
    linked_project: &LinkedProject,
    project: &queries::project::ProjectProject,
    environment: &str,
) -> Result<(Value, String)> {
    let environment_id = get_matched_environment(project, environment.to_string())?.id;
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
    let instance = find_service_instance(project, &environment_id, &service_id);
    let service_meta = if let Some(instance) = instance {
        if let Some(latest) = &instance.latest_deployment {
            if let Some(meta) = &latest.meta {
                let deploy = meta
                    .dot_get::<Value>("serviceManifest.deploy")?
                    .expect("Very old deployment, please redeploy");
                if let Some(c) = deploy.dot_get::<Value>("multiRegionConfig")? {
                    Some(c)
                } else if let Some(region) = deploy.dot_get::<Value>("region")? {
                    // old deployments only have numReplicas and a region field...
                    let mut map = Map::new();
                    let replicas = deploy.dot_get::<Value>("numReplicas")?.unwrap_or(json!(1));
                    map.insert(region.to_string(), json!({ "numReplicas": replicas }));
                    Some(json!({
                        "multiRegionConfig": map
                    }))
                } else {
                    None
                }
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
    // Check if scale is the actual subcommand (not just anywhere in args)
    // Handles: `railway scale` and `railway service scale`
    let args: Vec<String> = std::env::args().collect();
    let is_scale = args.len() >= 2
        && (args[1].eq_ignore_ascii_case("scale")
            || (args.len() >= 3
                && args[1].eq_ignore_ascii_case("service")
                && args[2].eq_ignore_ascii_case("scale")));
    if !is_scale {
        return cmd;
    }
    block_on(async move {
        let configs = Configs::new().unwrap();
        let client = GQLClient::new_authorized(&configs).unwrap();
        let regions = post_graphql::<queries::Regions, _>(
            &client,
            configs.get_backboard(),
            queries::regions::Variables,
        )
        .await
        .expect("couldn't get regions");

        // Collect region names as owned Strings.
        let region_strings = regions
            .regions
            .iter()
            .filter(|r| r.railway_metal.unwrap_or_default())
            .map(|r| r.name.to_string())
            .collect::<Vec<String>>();

        // Mutate the command to add each region as a flag.
        let mut new_cmd = cmd;
        for region in region_strings {
            let region_static: &'static str = Box::leak(region.into_boxed_str());
            new_cmd = new_cmd.arg(
                Arg::new(region_static) // unique identifier
                    .long(region_static) // --my-region
                    .help(format!("Number of instances to run on {region_static}"))
                    .value_name("INSTANCES")
                    .value_parser(clap::value_parser!(u64))
                    .action(clap::ArgAction::Set),
            );
        }
        new_cmd
    })
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
