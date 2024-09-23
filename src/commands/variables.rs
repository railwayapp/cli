use super::*;
use crate::{
    consts::TICK_STRING,
    controllers::{
        project::{ensure_project_and_environment_exist, get_project},
        variables::get_service_variables,
    },
    errors::RailwayError,
    table::Table,
};
use std::{collections::BTreeMap, time::Duration};

/// Show variables for active environment
#[derive(Parser)]
pub struct Args {
    /// The service to show/set variables for
    #[clap(short, long)]
    service: Option<String>,

    /// Show variables in KV format
    #[clap(short, long)]
    kv: bool,

    /// The "{key}={value}" environment variable pair to set the service variables.
    /// Example:
    ///
    /// ```bash
    /// railway variables --set "MY_SPECIAL_ENV_VAR=1" --set "BACKEND_PORT=3000"
    /// ```
    #[clap(long)]
    set: Vec<String>,
}

pub async fn command(args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    if !args.set.is_empty() {
        let variables: BTreeMap<String, String> = args
            .set
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

        let service_id = linked_project
            .service
            .clone()
            .ok_or_else(|| RailwayError::NoServiceLinked)?;

        let vars = mutations::variable_collection_upsert::Variables {
            project_id: linked_project.project.clone(),
            environment_id: linked_project.environment.clone(),
            service_id,
            variables,
        };

        post_graphql::<mutations::VariableCollectionUpsert, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?;

        spinner.finish_with_message(format!("Set {fmt_variables}"));
        return Ok(());
    }

    let _vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let (vars, name) = if let Some(ref service) = args.service {
        let service_name = project
            .services
            .edges
            .iter()
            .find(|edge| edge.node.id == *service || edge.node.name == *service)
            .ok_or_else(|| RailwayError::ServiceNotFound(service.clone()))?;
        (
            queries::variables_for_service_deployment::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: service_name.node.id.clone(),
            },
            service_name.node.name.clone(),
        )
    } else if let Some(ref service) = linked_project.service {
        let service_name = project
            .services
            .edges
            .iter()
            .find(|edge| edge.node.id == *service)
            .ok_or_else(|| RailwayError::ServiceNotFound(service.clone()))?;
        (
            queries::variables_for_service_deployment::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: service.clone(),
            },
            service_name.node.name.clone(),
        )
    } else {
        return Err(RailwayError::NoServiceLinked.into());
    };

    let variables = get_service_variables(
        &client,
        &configs,
        vars.project_id,
        vars.environment_id,
        vars.service_id,
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

    let table = Table::new(name, variables);
    table.print()?;

    Ok(())
}
