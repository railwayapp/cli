use std::{collections::BTreeMap, fmt::Display, time::Duration};

use crate::{
    consts::TICK_STRING,
    controllers::project::get_project,
    errors::RailwayError,
    interact_or,
    util::prompt::{
        fake_select, prompt_confirm_with_default, prompt_options, prompt_options_skippable,
        prompt_text, prompt_text_with_placeholder_disappear_skippable, PromptService,
    },
};
use anyhow::bail;
use is_terminal::IsTerminal;
use tokio::task::JoinHandle;

use super::{queries::project::ProjectProjectEnvironmentsEdgesNode, *};

/// Create, delete or link an environment
#[derive(Parser)]
pub struct Args {
    /// The environment to link to
    environment: Option<String>,

    #[clap(subcommand)]
    command: Option<EnvironmentCommand>,
}

#[derive(Subcommand)]
pub enum EnvironmentCommand {
    /// Create a new environment
    New(NewArgs),

    /// Delete an environment
    #[clap(visible_aliases = &["remove", "rm"])]
    Delete(DeleteArgs),
}

#[derive(Parser)]
pub struct NewArgs {
    /// The name of the environment to create
    pub name: Option<String>,

    /// The name of the environment to duplicate
    #[clap(long, short, visible_alias = "copy", visible_short_alias = 'c')]
    pub duplicate: Option<String>,

    /// Variables to assign in the new environment
    ///
    /// Note: This will only work if the environment is being duplicated, and that the service specified is present in the original environment
    ///
    /// Examples:
    ///
    /// railway environment new foo --duplicate bar --service-variable <service name/service uuid> BACKEND_PORT=3000
    #[clap(long = "service-variable", short = 'v', number_of_values = 2, value_names = &["SERVICE", "VARIABLE"])]
    pub service_variables: Vec<String>,
}

#[derive(Parser)]
pub struct DeleteArgs {
    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    bypass: bool,

    /// The environment to delete
    environment: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Some(EnvironmentCommand::New(args)) => new_environment(args).await,
        Some(EnvironmentCommand::Delete(args)) => delete_environment(args).await,
        None => link_environment(args).await,
    }
}

async fn new_environment(args: NewArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let project_id = project.id.clone();
    let is_terminal = std::io::stdout().is_terminal();

    let name = select_name_new(&args, is_terminal)?;
    let duplicate_id = select_duplicate_id_new(&args, &project, is_terminal)?;
    let service_variables =
        select_service_variables_new(args, &project, is_terminal, &duplicate_id)?;
    // create the environment!
    let vars = mutations::environment_create::Variables {
        project_id: project.id.clone(),
        name,
        source_id: duplicate_id,
    };

    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message("Creating environment...");
    spinner.enable_steady_tick(Duration::from_millis(100));

    let response =
        post_graphql::<mutations::EnvironmentCreate, _>(&client, &configs.get_backboard(), vars)
            .await?;
    spinner.finish_with_message(format!(
        "{} {} {}",
        "Environment".green(),
        response.environment_create.name.magenta().bold(),
        "created! 🎉".green()
    ));
    let env_id = response.environment_create.id.clone();
    let env_name = response.environment_create.name.clone();
    if !service_variables.is_empty() {
        upsert_variables(&configs, client, project, service_variables, env_id.clone()).await?;
    } else {
        println!();
    }

    configs.link_project(
        project_id,
        linked_project.name.clone(),
        env_id,
        Some(env_name),
    )?;

    Ok(())
}

async fn delete_environment(args: DeleteArgs) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let is_terminal = std::io::stdout().is_terminal();

    let (id, name) = if let Some(environment) = args.environment {
        if let Some(env) = project.environments.edges.iter().find(|e| {
            (e.node.id.to_lowercase() == environment)
                || (e.node.name.to_lowercase() == environment.to_lowercase())
        }) {
            fake_select("Select the environment to delete", &env.node.name);
            (env.node.id.clone(), env.node.name.clone())
        } else {
            bail!(RailwayError::EnvironmentNotFound(environment))
        }
    } else if is_terminal {
        let environments = project
            .environments
            .edges
            .iter()
            .map(|env| Environment(&env.node))
            .collect::<Vec<_>>();
        let r = prompt_options("Select the environment to delete", environments)?;
        (r.0.id.clone(), r.0.name.clone())
    } else {
        bail!("Environment must be specified when not running in a terminal");
    };

    if !args.bypass {
        let confirmed = prompt_confirm_with_default(
            format!(
                r#"Are you sure you want to delete the environment "{}"?"#,
                name.red()
            )
            .as_str(),
            false,
        )?;

        if !confirmed {
            return Ok(());
        }
    }

    let is_two_factor_enabled = {
        let vars = queries::two_factor_info::Variables {};

        let info =
            post_graphql::<queries::TwoFactorInfo, _>(&client, configs.get_backboard(), vars)
                .await?
                .two_factor_info;

        info.is_verified
    };
    if is_two_factor_enabled {
        let token = prompt_text("Enter your 2FA code")?;
        let vars = mutations::validate_two_factor::Variables { token };

        let valid =
            post_graphql::<mutations::ValidateTwoFactor, _>(&client, configs.get_backboard(), vars)
                .await?
                .two_factor_info_validate;

        if !valid {
            return Err(RailwayError::InvalidTwoFactorCode.into());
        }
    }
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message("Deleting environment...");
    spinner.enable_steady_tick(Duration::from_millis(100));
    let _r = post_graphql::<mutations::EnvironmentDelete, _>(
        &client,
        &configs.get_backboard(),
        mutations::environment_delete::Variables { id },
    )
    .await?;
    spinner.finish_with_message("Environment deleted!");
    Ok(())
}

async fn link_environment(args: Args) -> std::result::Result<(), anyhow::Error> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    if project.deleted_at.is_some() {
        bail!(RailwayError::ProjectDeleted);
    }

    let environments = project
        .environments
        .edges
        .iter()
        .map(|env| Environment(&env.node))
        .collect::<Vec<_>>();

    let environment = match args.environment {
        // If the environment is specified, find it in the list of environments
        Some(environment) => {
            let environment = environments
                .iter()
                .find(|env| {
                    env.0.id == environment
                        || env.0.name.to_lowercase() == environment.to_lowercase()
                })
                .context("Environment not found")?;
            environment.clone()
        }
        // If the environment is not specified, prompt the user to select one
        None => {
            interact_or!("Environment must be specified when not running in a terminal");
            let environment = if environments.len() == 1 {
                match environments.first() {
                    // Project has only one environment, so use that one
                    Some(environment) => environment.clone(),
                    // Project has no environments, so bail
                    None => bail!("Project has no environments"),
                }
            } else {
                // Project has multiple environments, so prompt the user to select one
                prompt_options("Select an environment", environments)?
            };
            environment
        }
    };

    let environment_name = environment.0.name.clone();
    println!("Activated environment {}", environment_name.purple().bold());

    configs.link_project(
        linked_project.project.clone(),
        linked_project.name.clone(),
        environment.0.id.clone(),
        Some(environment_name),
    )?;
    configs.write()?;
    Ok(())
}

async fn upsert_variables(
    configs: &Configs,
    client: reqwest::Client,
    project: queries::project::ProjectProject,
    service_variables: Vec<(String, (String, String))>,
    env_id: String,
) -> Result<(), anyhow::Error> {
    let good_vars: Vec<(String, BTreeMap<String, String>)> = service_variables
        .chunk_by(|a, b| a.0 == b.0) // group by service id
        .map(|vars| {
            let service = vars.first().unwrap().0.clone();
            let variables = vars
                .iter()
                .map(|v| (v.1 .0.clone(), v.1 .1.clone()))
                .collect::<BTreeMap<_, _>>();
            (service, variables)
        })
        .collect();
    let mut tasks: Vec<JoinHandle<Result<()>>> = Vec::new();
    for (service_id, variables) in good_vars {
        let client = client.clone();
        let project = project.id.clone();
        let env_id = env_id.clone();
        let backboard = configs.get_backboard();
        tasks.push(tokio::spawn(async move {
            let vars = mutations::variable_collection_upsert::Variables {
                project_id: project,
                environment_id: env_id,
                service_id,
                variables,
                skip_deploys: None,
            };
            let _response =
                post_graphql::<mutations::VariableCollectionUpsert, _>(&client, backboard, vars)
                    .await?;
            Ok(())
        }));
    }
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message("Inserting variables...");
    spinner.enable_steady_tick(Duration::from_millis(100));
    let r = futures::future::join_all(tasks).await;
    for r in r {
        r??;
    }
    spinner.finish_and_clear();
    Ok(())
}

fn select_service_variables_new(
    args: NewArgs,
    project: &queries::project::ProjectProject,
    is_terminal: bool,
    duplicate_id: &Option<String>,
) -> Result<Vec<(String, (String, String))>> {
    let service_variables = if let Some(ref duplicate_id) = *duplicate_id {
        let services = project
            .services
            .edges
            .iter()
            .filter(|s| {
                s.node
                    .service_instances
                    .edges
                    .iter()
                    .any(|i| &i.node.environment_id == duplicate_id)
            })
            .collect::<Vec<_>>();
        if !args.service_variables.is_empty() {
            args.service_variables
                .chunks(2)
                .filter_map(|chunk| {
                    // clap ensures that there will always be 2 values whenever the flag is provided
                    let service = chunk.first().unwrap();
                    let service_meta = services
                        .iter()
                        .find(|s| {
                            (s.node.id.to_lowercase() == service.to_lowercase())
                                || (s.node.name.to_lowercase() == service.to_lowercase())
                        })
                        .map(|s| (s.node.id.clone(), s.node.name.clone()));
                    let variable = chunk.last().unwrap().split('=').collect::<Vec<&str>>();
                    if service_meta.is_none() {
                        println!(
                            "{}: Service {} not found",
                            "Error".red().bold(),
                            service.blue()
                        );
                        std::process::exit(1); // returning errors in closures... oh my god...
                    }
                    service_meta.map(|service_meta| {
                        let key = variable.first().unwrap().to_string();
                        let value = variable.last().unwrap().to_string();
                        fake_select(
                            "Select a service to set variables for",
                            service_meta.1.as_str(),
                        );
                        fake_select("Enter a variable", format!("{}={}", key, value).as_str());
                        (
                            service_meta.0, // id
                            (key, value),
                        )
                    })
                })
                .collect::<Vec<(String, (String, String))>>()
        } else if is_terminal {
            let mut variables: Vec<(String, (String, String))> = Vec::new();
            let p_services = services
                .iter()
                .map(|s| PromptService(&s.node))
                .collect::<Vec<_>>();
            let mut used_services: Vec<&PromptService> = Vec::new();
            loop {
                let prompt_services: Vec<&PromptService<'_>> = p_services
                    .iter()
                    .filter(|p| !used_services.contains(p))
                    .clone()
                    .collect();
                if prompt_services.is_empty() {
                    break;
                }
                let service = prompt_options_skippable(
                    "Select a service to set variables for <esc to skip>",
                    prompt_services,
                )?;
                if let Some(service) = service {
                    loop {
                        // prompt for value now
                        let variable = prompt_text_with_placeholder_disappear_skippable(
                            "Enter a variable",
                            "<KEY=VALUE, press esc to skip>",
                        )?;
                        if let Some(variable) = variable {
                            let variable = variable.split('=').collect::<Vec<&str>>();
                            if variable.len() != 2 || variable[1].is_empty() {
                                println!("{}: Invalid variable format", "Warn".yellow().bold());
                                continue;
                            }
                            variables.push((
                                service.0.id.clone(),
                                (
                                    variable.first().unwrap().to_string(),
                                    variable.last().unwrap().to_string(),
                                ),
                            ));
                        } else {
                            break;
                        }
                    }
                    used_services.push(service)
                } else {
                    break;
                }
            }
            variables
        } else {
            vec![]
        }
    } else if !args.service_variables.is_empty() {
        // if duplicate id is None and service_variables are provided, error
        bail!("Service variables can only be set when duplicating an environment")
    } else {
        vec![]
    };
    Ok(service_variables)
}

fn select_duplicate_id_new(
    args: &NewArgs,
    project: &queries::project::ProjectProject,
    is_terminal: bool,
) -> Result<Option<String>, anyhow::Error> {
    let duplicate_id = if let Some(ref duplicate) = args.duplicate {
        let env = project.environments.edges.iter().find(|env| {
            (env.node.name.to_lowercase() == duplicate.to_lowercase())
                || (env.node.id == *duplicate)
        });
        if let Some(env) = env {
            fake_select("Duplicate from", &env.node.name);
            Some(env.node.id.clone())
        } else {
            bail!(RailwayError::EnvironmentNotFound(duplicate.clone()))
        }
    } else if is_terminal {
        let environments = project
            .environments
            .edges
            .iter()
            .map(|env| Environment(&env.node))
            .collect::<Vec<_>>();
        prompt_options_skippable(
            "Duplicate from <esc to create an empty environment>",
            environments,
        )?
        .map(|e| e.0.id.clone())
    } else {
        None
    };
    Ok(duplicate_id)
}

fn select_name_new(args: &NewArgs, is_terminal: bool) -> Result<String, anyhow::Error> {
    let name = if let Some(name) = args.name.clone() {
        fake_select("Environment name", name.as_str());
        name
    } else if is_terminal {
        loop {
            let q = prompt_text("Environment name")?;
            if q.is_empty() {
                println!(
                    "{}: Environment name cannot be empty",
                    "Warn".yellow().bold()
                );
                continue;
            } else {
                break q;
            }
        }
    } else {
        bail!("Environment name must be specified when not running in a terminal");
    };
    Ok(name)
}

#[derive(Debug, Clone)]
struct Environment<'a>(&'a ProjectProjectEnvironmentsEdgesNode);

impl Display for Environment<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}
