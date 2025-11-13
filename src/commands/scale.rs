use crate::{
    consts::TICK_STRING,
    controllers::environment::get_matched_environment,
    util::prompt::{
        prompt_select_with_cancel, prompt_u64_with_placeholder_and_validation_and_cancel,
    },
};
use anyhow::bail;
use clap::{Arg, Command, Parser};
use country_emoji::flag;
use futures::executor::block_on;
use is_terminal::IsTerminal;
use json_dotpath::DotPaths as _;
use serde_json::{Map, Value, json};
use std::{cmp::Ordering, collections::HashMap, fmt::Display, time::Duration};
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
    let existing = get_existing_config(&args, &linked_project, project, environment)?;
    let new_config = convert_hashmap_into_map(
        if args.dynamic.0.is_empty() && std::io::stdout().is_terminal() {
            prompt_for_regions(&configs, &client, &existing).await?
        } else if args.dynamic.0.is_empty() {
            bail!("Please specify regions via the flags when not running in a terminal")
        } else {
            args.dynamic.0
        },
    );
    if new_config.is_empty() {
        println!("No changes made");
        return Ok(());
    }
    let region_data = merge_config(existing, new_config);
    update_regions_and_redeploy(configs, client, linked_project, region_data).await?;

    Ok(())
}

async fn prompt_for_regions(
    configs: &Configs,
    client: &reqwest::Client,
    existing: &Value,
) -> Result<HashMap<String, u64>> {
    let mut updated: HashMap<String, u64> = HashMap::new();
    let mut regions = post_graphql::<queries::Regions, _>(
        client,
        configs.get_backboard(),
        queries::regions::Variables,
    )
    .await
    .expect("couldn't get regions");
    loop {
        let get_replicas_amount = |name: String| {
            let before = if let Some(num) = existing.get(name.clone()) {
                num.get("numReplicas").unwrap().as_u64().unwrap() // fine to unwrap, API only returns ones that have a replica
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
        let regions = regions
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
        let p = prompt_select_with_cancel("Select a region <esc to finish>", regions)?;
        if let Some(region) = p {
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
            } else {
                // esc pressed when entering number, go back to selecting regions
                continue;
            }
        } else {
            // they pressed esc to cancel
            break;
        }
    }
    Ok(updated.clone())
}

async fn update_regions_and_redeploy(
    configs: Configs,
    client: reqwest::Client,
    linked_project: LinkedProject,
    region_data: Value,
) -> Result<(), anyhow::Error> {
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message("Updating regions...");
    spinner.enable_steady_tick(Duration::from_millis(100));
    post_graphql::<mutations::UpdateRegions, _>(
        &client,
        configs.get_backboard(),
        mutations::update_regions::Variables {
            environment_id: linked_project.environment.clone(),
            service_id: linked_project.service.clone().unwrap(),
            multi_region_config: region_data,
        },
    )
    .await?;
    spinner.finish_with_message("Regions updated");
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message("Redeploying...");
    spinner.enable_steady_tick(Duration::from_millis(100));
    post_graphql::<mutations::ServiceInstanceDeploy, _>(
        &client,
        configs.get_backboard(),
        mutations::service_instance_deploy::Variables {
            environment_id: linked_project.environment,
            service_id: linked_project.service.unwrap(),
        },
    )
    .await?;
    spinner.finish_with_message("Redeployed");
    Ok(())
}

fn merge_config(existing: Value, new_config: Map<String, Value>) -> Value {
    let mut map = match existing {
        Value::Object(object) => object,
        _ => unreachable!(), // will always be a map
    };
    map.extend(new_config);
    Value::Object(map)
}

fn convert_hashmap_into_map(map: HashMap<String, u64>) -> Map<String, Value> {
    let new_config = map.iter().fold(Map::new(), |mut map, (key, val)| {
        map.insert(
            key.clone(),
            if *val == 0 {
                Value::Null // this is how the dashboard does it
            } else {
                json!({ "numReplicas": val })
            },
        );
        map
    });
    new_config
}

fn get_existing_config(
    args: &Args,
    linked_project: &LinkedProject,
    project: queries::project::ProjectProject,
    environment: String,
) -> Result<Value> {
    let environment_id = get_matched_environment(&project, environment)?.id;
    let service_input: &String = args.service.as_ref().unwrap_or(linked_project.service.as_ref().expect("No service linked. Please either specify a service with the --service flag or link one with `railway service`"));
    let service_meta = if let Some(service) = project.services.edges.iter().find(|p| {
        (p.node.id == *service_input)
            || (p.node.name.to_lowercase() == service_input.to_lowercase())
    }) {
        // check that service exists in that environment
        if let Some(instance) = service
            .node
            .service_instances
            .edges
            .iter()
            .find(|p| p.node.environment_id == environment_id)
        {
            if let Some(latest) = &instance.node.latest_deployment {
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
        }
    } else {
        None
    };
    Ok(service_meta.unwrap_or(Value::Object(Map::new())))
}

/// This function generates flags that are appended to the command at runtime.
pub fn get_dynamic_args(cmd: Command) -> Command {
    if !std::env::args().any(|f| f.eq_ignore_ascii_case("scale")) {
        // if the command has nothing to do with railway scale, dont make the web request.
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
                    .value_parser(clap::value_parser!(u16))
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
            if key == "json" || Args::FIELD_NAMES_AS_ARRAY.contains(&key.as_str()) {
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

/// Formatting done manually
pub struct PromptRegion(pub queries::regions::RegionsRegions, pub String);

impl Display for PromptRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.1)
    }
}
