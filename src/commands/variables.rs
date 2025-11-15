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
use std::{
    collections::BTreeMap,
    io::{stdin, BufRead, IsTerminal},
    time::Duration,
};

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

    /// Read environment variable pairs from stdin.
    ///
    /// Each line should contain exactly one "{KEY}={VALUE}"" pair. Leading and trailing whitespace is trimmed.
    /// Empty lines are ignored. If combined with --set, values from both sources are applied.
    ///
    /// Examples:
    ///
    ///     # Read a single variable from stdin
    ///
    ///     echo "FOO=bar" | railway variables --set-from-stdin
    ///
    ///     # Read multiple variables, one per line
    ///
    ///     printf "FOO=bar\nBAZ=qux\n" | railway variables --set-from-stdin
    ///
    ///     # Load variables from a .env file
    ///
    ///     cat .env | railway variables --set-from-stdin
    #[clap(long, verbatim_doc_comment)]
    set_from_stdin: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// Skip triggering deploys when setting variables
    #[clap(long)]
    skip_deploys: bool,
}

pub async fn command(mut args: Args) -> Result<()> {
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

    if args.set_from_stdin {
        let stdin = stdin();

        if stdin.is_terminal() {
            bail!("--set-from-stdin requires input from stdin (e.g., via pipe or redirect)");
        }

        for line in stdin.lock().lines() {
            let line = line?;

            if !line.trim().is_empty() {
                args.set.push(line.trim().to_string());
            }
        }
    }

    if !args.set.is_empty() {
        set_variables(
            args.set,
            linked_project.project.clone(),
            environment_id,
            service_id,
            &client,
            &configs,
            args.skip_deploys,
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
            println!("{key}={value}");
        }
        return Ok(());
    }

    if args.json {
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
    skip_deploys: bool,
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
        skip_deploys: if skip_deploys { Some(true) } else { None },
    };
    post_graphql::<mutations::VariableCollectionUpsert, _>(client, configs.get_backboard(), vars)
        .await?;
    spinner.finish_with_message(format!("Set variables {fmt_variables}"));
    Ok(())
}
