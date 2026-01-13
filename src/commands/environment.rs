use std::{collections::BTreeMap, fmt::Display, time::Duration};

use crate::{
    consts::TICK_STRING,
    controllers::project::{get_project, get_service_ids_in_env},
    controllers::variables::Variable,
    errors::RailwayError,
    interact_or,
    util::{
        prompt::{
            PromptService, fake_select, prompt_confirm_with_default, prompt_options,
            prompt_options_skippable, prompt_text, prompt_variables,
        },
        retry::{RetryConfig, retry_with_backoff},
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
    /// Link an environment to the current project
    Link(LinkArgs),

    /// Create a new environment
    New(NewArgs),

    /// Delete an environment
    #[clap(visible_aliases = &["remove", "rm"])]
    Delete(DeleteArgs),
}

#[derive(Parser)]
pub struct LinkArgs {
    /// The environment to link to
    environment: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
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
        Some(EnvironmentCommand::Link(link_args)) => {
            link_environment(link_args.environment, link_args.json).await
        }
        Some(EnvironmentCommand::New(new_args)) => new_environment(new_args).await,
        Some(EnvironmentCommand::Delete(delete_args)) => delete_environment(delete_args).await,
        // Legacy: `railway environment <name>` without subcommand
        None => link_environment(args.environment, false).await,
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
    // Use background processing when duplicating to avoid timeouts
    let apply_changes_in_background = duplicate_id.is_some();

    let vars = mutations::environment_create::Variables {
        project_id: project.id.clone(),
        name,
        source_id: duplicate_id,
        apply_changes_in_background: Some(apply_changes_in_background),
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

    let env_id = response.environment_create.id.clone();
    let env_name = response.environment_create.name.clone();

    if apply_changes_in_background {
        // Wait for background duplication to complete
        let _ = wait_for_environment_creation(&client, &configs, env_id.clone()).await;
    }

    spinner.finish_with_message(format!(
        "{} {} {}",
        "Environment".green(),
        env_name.magenta().bold(),
        "created! 🎉".green()
    ));
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
        let all_environments = &project.environments.edges;
        let environments = all_environments
            .iter()
            .filter(|env| env.node.can_access)
            .map(|env| Environment(&env.node))
            .collect::<Vec<_>>();
        if environments.is_empty() {
            if all_environments.is_empty() {
                bail!("Project has no environments");
            } else {
                bail!("All environments in this project are restricted");
            }
        }
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

async fn link_environment(
    environment_arg: Option<String>,
    json: bool,
) -> std::result::Result<(), anyhow::Error> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    if project.deleted_at.is_some() {
        bail!(RailwayError::ProjectDeleted);
    }

    let all_environments = &project.environments.edges;
    let environments = all_environments
        .iter()
        .filter(|env| env.node.can_access)
        .map(|env| Environment(&env.node))
        .collect::<Vec<_>>();

    if environments.is_empty() {
        if all_environments.is_empty() {
            bail!("Project has no environments");
        } else {
            bail!("All environments in this project are restricted");
        }
    }

    let environment = match environment_arg {
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

            if environments.len() == 1 {
                match environments.first() {
                    // Project has only one environment, so use that one
                    Some(environment) => environment.clone(),
                    // Project has no environments, so bail
                    None => bail!("Project has no environments"),
                }
            } else {
                // Project has multiple environments, so prompt the user to select one
                prompt_options("Select an environment", environments)?
            }
        }
    };

    let environment_id = environment.0.id.clone();
    let environment_name = environment.0.name.clone();

    if json {
        println!(
            "{}",
            serde_json::json!({"id": environment_id, "name": environment_name})
        );
    } else {
        println!("Activated environment {}", environment_name.purple().bold());
    }

    configs.link_project(
        linked_project.project.clone(),
        linked_project.name.clone(),
        environment_id,
        Some(environment_name),
    )?;
    configs.write()?;
    Ok(())
}

async fn upsert_variables(
    configs: &Configs,
    client: reqwest::Client,
    project: queries::project::ProjectProject,
    service_variables: Vec<(String, Variable)>,
    env_id: String,
) -> Result<(), anyhow::Error> {
    let good_vars: Vec<(String, BTreeMap<String, String>)> = service_variables
        .chunk_by(|a, b| a.0 == b.0) // group by service id
        .map(|vars| {
            let service = vars.first().unwrap().0.clone();
            let variables = vars
                .iter()
                .map(|v| (v.1.key.clone(), v.1.value.clone()))
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
) -> Result<Vec<(String, Variable)>> {
    let service_variables = if let Some(ref duplicate_id) = *duplicate_id {
        let service_ids_in_env = get_service_ids_in_env(project, duplicate_id);
        let services = project
            .services
            .edges
            .iter()
            .filter(|s| service_ids_in_env.contains(&s.node.id))
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

                    let variable = match chunk.last().unwrap().parse::<Variable>() {
                        Ok(v) => v,
                        Err(e) => {
                            println!("{e:?}");
                            return None;
                        }
                    };
                    if service_meta.is_none() {
                        println!(
                            "{}: Service {} not found",
                            "Error".red().bold(),
                            service.blue()
                        );
                        std::process::exit(1); // returning errors in closures... oh my god...
                    }
                    service_meta.map(|service_meta| {
                        fake_select(
                            "Select a service to set variables for",
                            service_meta.1.as_str(),
                        );
                        fake_select(
                            "Enter a variable",
                            format!("{}={}", variable.key, variable.value).as_str(),
                        );
                        (
                            service_meta.0, // id
                            variable,
                        )
                    })
                })
                .collect::<Vec<(String, Variable)>>()
        } else if is_terminal {
            let mut variables: Vec<(String, Variable)> = Vec::new();
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
                    variables.extend(
                        prompt_variables()?
                            .into_iter()
                            .map(|f| (service.0.id.to_owned(), f))
                            .collect::<Vec<(String, Variable)>>(),
                    );
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
            .filter(|env| env.node.can_access)
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

// Polls for environment creation completion when using background processing.
// Returns true when the environment patch status reaches "STAGED" state.
async fn wait_for_environment_creation(
    client: &reqwest::Client,
    configs: &Configs,
    environment_id: String,
) -> Result<bool> {
    let env_id = environment_id;
    let check_status = || async {
        let vars = queries::environment_staged_changes::Variables {
            environment_id: env_id.clone(),
        };

        let response = post_graphql::<queries::EnvironmentStagedChanges, _>(
            client,
            configs.get_backboard(),
            vars,
        )
        .await?;

        let status = &response.environment_staged_changes.status;

        // Check if environment duplication has completed
        use queries::environment_staged_changes::EnvironmentPatchStatus;
        match status {
            EnvironmentPatchStatus::STAGED | EnvironmentPatchStatus::COMMITTED => Ok(true),
            EnvironmentPatchStatus::APPLYING => bail!("Still applying changes"),
            _ => bail!("Unexpected status: {:?}", status),
        }
    };

    let config = RetryConfig {
        max_attempts: 40,        // ~2 minutes with exponential backoff
        initial_delay_ms: 1000,  // Start at 1 second
        max_delay_ms: 5000,      // Cap at 5 seconds
        backoff_multiplier: 1.5, // Exponential backoff
        on_retry: None,
    };

    retry_with_backoff(config, check_status).await
}

#[derive(Debug, Clone)]
struct Environment<'a>(&'a ProjectProjectEnvironmentsEdgesNode);

impl Display for Environment<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}
