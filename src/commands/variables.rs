use super::*;
use crate::{
    consts::TICK_STRING,
    controllers::{
        environment::get_matched_environment,
        project::{ensure_project_and_environment_exist, get_project},
        variables::get_service_variables,
    },
    errors::RailwayError,
    table::Table,
};
use anyhow::bail;
use std::{collections::BTreeMap, time::Duration};

/// Show variables for active environment
#[derive(Parser)]
pub struct Args {
    /// The service to show/set variables for
    #[clap(short, long)]
    service: Option<String>,

    /// The environment to show/set variables for
    #[clap(short, long)]
    environment: Option<String>,

    /// Show variables in KV format
    #[clap(short, long)]
    kv: bool,

    /// The "{key}={value}" environment variable pair to set the service variables.
    /// Example:
    ///
    /// railway variables --set "MY_SPECIAL_ENV_VAR=1" --set "BACKEND_PORT=3000"
    #[clap(long)]
    set: Vec<String>,
}

pub async fn command(args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    let services = project.services.edges.iter().collect::<Vec<_>>();

    let environment_id = get_matched_environment(&project, environment)?.id;
    let service_id = match (args.service, linked_project.service) {
        // If the user specified a service, use that
        (Some(service_arg), _) => services
            .iter()
            .find(|service| service.node.name == service_arg || service.node.id == service_arg)
            .with_context(|| format!("Service '{service_arg}' not found"))?
            .node
            .id
            .to_owned(),
        // Otherwise if we have a linked service, use that
        (_, Some(linked_service)) => linked_service,
        // Otherwise it's a user error
        _ => bail!(RailwayError::NoServiceLinked),
    };

    if !args.set.is_empty() {
        set_variables(
            args.set,
            linked_project.project.clone(),
            environment_id,
            service_id,
            &client,
            &configs,
        )
        .await?;
        return Ok(());
    }

    let variables = get_service_variables(
        &client,
        &configs,
        project.id,
        environment_id,
        service_id.clone(),
    )
    .await?;

    if variables.is_empty() {
        eprintln!("No variables found");
        return Ok(());
    }

    if args.kv {
        for (key, value) in variables {
            println!("{}={}", key, value);
        }
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&variables)?);
        return Ok(());
    }

    let table = Table::new(
        services
            .iter()
            .find(|s| s.node.id == service_id)
            .unwrap()
            .node
            .name
            .clone(),
        variables,
    );
    table.print()?;

    Ok(())
}

async fn set_variables(
    set: Vec<String>,
    project: String,
    environment_id: String,
    service_id: String,
    client: &reqwest::Client,
    configs: &Configs,
) -> Result<(), anyhow::Error> {
    let variables: BTreeMap<String, String> = set
        .iter()
        .filter_map(|v| {
            let mut split = v.split('=');
            let key = split.next()?.trim().to_owned();
            let value = split.collect::<Vec<&str>>().join("=").trim().to_owned();
            if value.is_empty() {
                None
            } else {
                Some((key, value))
            }
        })
        .collect();
    let fmt_variables = variables
        .keys()
        .map(|k| k.bold().to_string())
        .collect::<Vec<String>>()
        .join(", ");
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")
                .expect("Failed to set spinner template"),
        )
        .with_message(format!("Setting {fmt_variables}..."));
    spinner.enable_steady_tick(Duration::from_millis(100));
    let vars = mutations::variable_collection_upsert::Variables {
        project_id: project,
        environment_id,
        service_id,
        variables,
    };
    post_graphql::<mutations::VariableCollectionUpsert, _>(client, configs.get_backboard(), vars)
        .await?;
    spinner.finish_with_message(format!("Set variables {fmt_variables}"));
    Ok(())
}
