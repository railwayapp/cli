use std::{
    collections::{BTreeMap, HashMap},
    fmt::Display,
    str::FromStr,
    time::Duration,
};

use crate::{
    consts::TICK_STRING,
    controllers::{project::get_project, variables::Variable},
    errors::RailwayError,
    interact_or,
    util::{
        prompt::{
            PromptServiceInstance, fake_select, prompt_confirm_with_default, prompt_multi_options,
            prompt_multi_options_with_defaults, prompt_options, prompt_options_skippable,
            prompt_text, prompt_variables,
        },
        retry::{RetryConfig, retry_with_backoff},
    },
};
use anyhow::bail;
use derive_more::Display as DeriveDisplay;
use is_terminal::IsTerminal;
use strum::{Display, EnumDiscriminants, EnumIter, EnumString, IntoEnumIterator, VariantNames};
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

    /// Assign services new sources in the new environment
    ///
    /// GitHub repo format: <owner>/<repo>/<branch>
    ///
    /// Docker image format: [optional registry url]/<owner>[/repo][:tag]
    ///
    /// Examples:
    ///
    /// railway environment new foo --duplicate bar --service-source <service name/service uuid> docker ubuntu:latest
    ///
    /// railway environment new foo --duplicate bar --service-source <service name/service uuid> github nodejs/node/branch
    #[clap(long = "service-source", short = 's', number_of_values = 3, value_names = &["SERVICE", "PLATFORM", "SOURCE"])]
    pub service_sources: Vec<String>,
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

    let changes = if let Some(ref duplicate_id) = duplicate_id {
        edit_services_select(&args, &project, duplicate_id.clone())?
    } else {
        Vec::new()
    };
    let apply_in_background = !changes.is_empty();

    let vars = mutations::environment_create::Variables {
        project_id: project.id.clone(),
        name,
        source_id: duplicate_id,
        apply_changes_in_background: Some(apply_in_background),
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

    if apply_in_background {
        // Wait for background duplication to complete
        spinner.set_message("Waiting for environment to duplicate...");
        let _ = wait_for_environment_creation(&client, &configs, env_id.clone()).await;
    }

    spinner.finish_with_message(format!(
        "{} {} {}",
        "Environment".green(),
        env_name.magenta().bold(),
        "created! 🎉".green()
    ));

    // let (variables, sources) = tokio::join!(
    //     upsert_variables(
    //         &configs,
    //         client.clone(),
    //         project.clone(),
    //         service_variables,
    //         env_id.clone()
    //     ),
    //     upsert_sources(
    //         &configs,
    //         client,
    //         project.id.clone(),
    //         service_sources,
    //         env_id.clone()
    //     )
    // );

    // variables?;
    // sources?;

    configs.link_project(
        project_id,
        linked_project.name.clone(),
        env_id,
        Some(env_name),
    )?;

    Ok(())
}

#[derive(Clone, DeriveDisplay, EnumDiscriminants)]
#[strum_discriminants(derive(Display, EnumIter, VariantNames), name(ChangeOption))]
/// In order to add a new Change that can be configured, add a variant to this enum and give it it's own type
/// Implement the ChangeHandler trait for the type
/// Add that type to the invocation of the register_handlers! macro
pub enum Change {
    #[strum_discriminants(strum(serialize = "Variables"))]
    Variable(Variable),
    #[strum_discriminants(strum(serialize = "Sources"))]
    Source(Source),
}

impl Change {
    pub fn variant_name(&self) -> String {
        ChangeOption::from(self)
            .to_string()
            .trim_end_matches('s')
            .to_lowercase()
    }
}

/// Trait for handling the parsing of a change
trait ChangeHandler: Clone {
    /// Get the command-line args for this change type
    fn get_args(args: &NewArgs) -> &Vec<String>;

    /// Parse from non-interactive arguments
    fn parse_non_interactive(args: Vec<String>) -> Vec<(String, Self)>;

    /// Parse interactively for a specific service
    fn parse_interactive(service_name: &str) -> Result<Vec<Self>>;

    /// Convert to Change enum
    fn into_change(self) -> Change;
}

macro_rules! register_handlers {
    ($($type:ident),* $(,)?) => {
        impl ChangeOption {
            fn get_args<'a>(&self, args: &'a NewArgs) -> &'a Vec<String> {
                match self {
                    $(ChangeOption::$type => <$type>::get_args(args),)*
                }
            }

            fn parse_non_interactive(&self, args: Vec<String>) -> Vec<(String, Change)> {
                match self {
                    $(
                        ChangeOption::$type => {
                            <$type>::parse_non_interactive(args)
                                .into_iter()
                                .map(|(s, item)| (s, item.into_change()))
                                .collect()
                        }
                    )*
                }
            }

            fn parse_interactive(&self, service_name: &str) -> Result<Vec<Change>> {
                match self {
                    $(
                        ChangeOption::$type => {
                            Ok(<$type>::parse_interactive(service_name)?
                                .into_iter()
                                .map(|item| item.into_change())
                                .collect())
                        }
                    )*
                }
            }
        }
    };
}

// Register all handlers (generates helper functions)
register_handlers!(Variable, Source);

/// environment id should be the id of the environment being duplicated if being used in new command
pub fn edit_services_select(
    args: &NewArgs,
    project: &queries::project::ProjectProject,
    environment_id: String,
) -> Result<Vec<(String, Change)>> {
    let is_terminal = std::io::stdout().is_terminal();

    // get all options and their respective non-interactive arguments
    let configure_options = ChangeOption::iter()
        .map(|opt| (opt, opt.get_args(args)))
        .collect::<Vec<(ChangeOption, &Vec<String>)>>();
    // find which have arguments provided non interactively
    let non_interactive_provided = configure_options
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .collect::<Vec<&(ChangeOption, &Vec<String>)>>();
    let selected = match (
        is_terminal,
        non_interactive_provided.is_empty(),
        non_interactive_provided.len() == configure_options.len(),
    ) {
        // not a terminal, options provided non-interactively
        // or: a terminal, but every option available is provided non-interactively
        (false, false, _) | (_, _, true) => {
            fake_select(
                "What do you want to configure?",
                &non_interactive_provided
                    .iter()
                    .map(|(opt, _)| opt.to_string())
                    .collect::<Vec<String>>()
                    .join(", "),
            );
            non_interactive_provided
                .iter()
                .map(|(c, _)| c.to_owned())
                .collect::<Vec<ChangeOption>>()
        }
        // is a terminal, if options have been provided non-interactively have those selected
        (true, _, _) => prompt_multi_options_with_defaults(
            "What do you want to configure? <enter to skip>",
            configure_options
                .to_vec()
                .iter()
                .cloned()
                .map(|(c, enabled)| (c, !enabled.is_empty()))
                .collect::<Vec<_>>(),
        )?,
        // not a terminal, nothing provided non-interactively, assume that no changes are wanted (early return)
        (false, true, _) => return Ok(Vec::new()),
    };
    // first, get the services that are in the environment
    // safe to unwrap (environment id provided is either a duplication id or an existing one)
    let services = &project
        .environments
        .edges
        .iter()
        .find(|env| env.node.id == environment_id)
        .unwrap()
        .node
        .service_instances
        .edges;
    // we now have the options that are wanted to be configured, go through them one by one and select the services
    let mut changes_final: Vec<(String, Change)> = Vec::new();
    for change in selected {
        // find non_interactive argument:
        let non_interactive_args = non_interactive_provided
            .iter()
            .find(|(option, _)| *option == change)
            .map(|(_, c)| (**c).clone())
            .unwrap_or_default();
        // non interactive isnt empty
        let changes = if !non_interactive_args.is_empty() {
            // each function needs to handle non interactive arguments
            // each function is responsible for checking what is provided is valid (e.g a variable is in the right format)
            // if it isn't valid, it should simply skip that parse - make sure this is known to the user
            // (see non_interactive_variables fn for skipping)
            let mut service_lookup: HashMap<&String, &String> = HashMap::new();
            let parsed = change
                .parse_non_interactive(non_interactive_args)
                .iter()
                // take all non interactive input where the actual input (not service) is valid
                .filter_map(|(service_input, change)| {
                    services
                        .iter()
                        .find(|service| {
                            // attempt to find service based on what is provided
                            (service.node.service_id.to_lowercase() == service_input.to_lowercase())
                                || (service.node.service_name.to_lowercase()
                                    == service_input.to_lowercase())
                        })
                        .map(|service| {
                            service_lookup
                                .insert(&service.node.service_id, &service.node.service_name);
                            (service.node.service_id.clone(), change.clone())
                        })
                        .or_else(|| {
                            // if can't be found, error and skip
                            eprintln!("Service {service_input} is not a valid name/id, skipping");
                            None
                        })
                })
                .collect::<Vec<(String, Change)>>();
            fake_select(
                &format!("What services do you want to configure? ({})", change),
                &service_lookup
                    .values()
                    .map(|s| s.as_str())
                    .collect::<Vec<&str>>()
                    .join(", "),
            );
            // this is for fake selects - we need to group by the service and then print the changes.
            parsed.chunk_by(|(a, _), (b, _)| a == b).for_each(|chunk| {
                chunk.iter().for_each(|(service, change)| {
                    fake_select(
                        &format!(
                            "Enter a {} for {}",
                            change.variant_name(),
                            service_lookup.get(service).unwrap()
                        ),
                        &change.to_string(),
                    )
                })
            });
            parsed
        } else if is_terminal {
            let prompt_services = services
                .iter()
                .map(|f| PromptServiceInstance(&f.node))
                .collect::<Vec<PromptServiceInstance>>();
            // find
            let selected_services = prompt_multi_options(
                &format!("What services do you want to configure? ({})", change),
                prompt_services.clone(),
            )?;
            let mut changes_interactive: Vec<(String, Change)> = Vec::new();

            for service in selected_services {
                let service = service.0;
                let parsed = change
                    .parse_interactive(&service.service_name)?
                    .into_iter()
                    .map(|c| (service.service_id.clone(), c));
                changes_interactive.extend(parsed);
            }
            changes_interactive
        } else {
            // not a terminal, somehow provided an option non-interactively without any values (clap ensures this doesn't happen)
            // but we still need to defualt to an empty Vec
            Vec::new()
        };
        changes_final.extend(changes.into_iter());
    }

    Ok(changes_final)
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

async fn link_environment(args: Args) -> std::result::Result<(), anyhow::Error> {
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

// async fn upsert_variables(
//     configs: &Configs,
//     client: reqwest::Client,
//     project: queries::project::ProjectProject,
//     service_variables: Vec<(String, Variable)>,
//     env_id: String,
// ) -> Result<(), anyhow::Error> {
//     if service_variables.is_empty() {
//         return Ok(());
//     }
//     let good_vars: Vec<(String, BTreeMap<String, String>)> = service_variables
//         .chunk_by(|a, b| a.0 == b.0) // group by service id
//         .map(|vars| {
//             let service = vars.first().unwrap().0.clone();
//             let variables = vars
//                 .iter()
//                 .map(|v| (v.1.key.clone(), v.1.value.clone()))
//                 .collect::<BTreeMap<_, _>>();
//             (service, variables)
//         })
//         .collect();
//     let mut tasks: Vec<JoinHandle<Result<()>>> = Vec::new();
//     for (service_id, variables) in good_vars {
//         let client = client.clone();
//         let project = project.id.clone();
//         let env_id = env_id.clone();
//         let backboard = configs.get_backboard();
//         tasks.push(tokio::spawn(async move {
//             let vars = mutations::variable_collection_upsert::Variables {
//                 project_id: project,
//                 environment_id: env_id,
//                 service_id,
//                 variables,
//                 skip_deploys: None,
//             };
//             let _response =
//                 post_graphql::<mutations::VariableCollectionUpsert, _>(&client, backboard, vars)
//                     .await?;
//             Ok(())
//         }));
//     }
//     let spinner = indicatif::ProgressBar::new_spinner()
//         .with_style(
//             indicatif::ProgressStyle::default_spinner()
//                 .tick_chars(TICK_STRING)
//                 .template("{spinner:.green} {msg}")?,
//         )
//         .with_message("Inserting variables...");
//     spinner.enable_steady_tick(Duration::from_millis(100));
//     let r = futures::future::join_all(tasks).await;
//     for r in r {
//         r??;
//     }
//     spinner.finish_and_clear();
//     Ok(())
// }

// async fn upsert_sources(
//     configs: &Configs,
//     client: reqwest::Client,
//     project: String,
//     service_sources: Vec<(String, Source)>,
//     env_id: String,
// ) -> Result<(), anyhow::Error> {
//     if service_sources.is_empty() {
//         return Ok(());
//     }
//     let project = get_project(&client, configs, project).await?;
//     let mut tasks: Vec<JoinHandle<Result<()>>> = Vec::new();
//     for (service_id, source) in service_sources {
//         let client = client.clone();
//         let project = project.clone();
//         let env_id = env_id.clone();
//         let backboard = configs.get_backboard();
//         tasks.push(tokio::spawn(async move {
//             /*
//             first check if there is a deployment trigger for the service in the environment
//             if there is, and the change is a github repository (match by enum type), then update the deployment trigger along with the
//             source (via service source and serviceinstanceupdate)
//             if the change is docker and the trigger, delete it
//              */
//             let Some(environment) = project
//                 .environments
//                 .edges
//                 .iter()
//                 .find(|a| a.node.id == env_id)
//             else {
//                 bail!("Environment couldn't be found, matched {env_id}");
//             };
//             let trigger = environment
//                 .node
//                 .deployment_triggers
//                 .edges
//                 .iter()
//                 .find(|t| t.node.service_id == Some(service_id.clone()))
//                 .map(|t| t.node.id.clone());

//             match source {
//                 Source::GitHub {
//                     owner,
//                     repo,
//                     branch,
//                 } => {
//                     let repository = format!("{owner}/{repo}");
//                     if let Some(id) = trigger {
//                         // trigger already exists, update
//                         post_graphql::<mutations::DeploymentTriggerUpdate, _>(
//                             &client,
//                             backboard.clone(),
//                             mutations::deployment_trigger_update::Variables {
//                                 id,
//                                 repository: repository.clone(),
//                                 branch,
//                             },
//                         )
//                         .await?;
//                     } else {
//                         // trigger does not exist, so we need to make one
//                         post_graphql::<mutations::DeploymentTriggerCreate, _>(
//                             &client,
//                             backboard.clone(),
//                             mutations::deployment_trigger_create::Variables {
//                                 service_id: service_id.clone(),
//                                 environment_id: env_id.clone(),
//                                 project_id: project.id.clone(),
//                                 repository: repository.clone(),
//                                 branch,
//                                 provider: String::from("github"),
//                             },
//                         )
//                         .await?;
//                     }
//                     post_graphql::<mutations::ServiceInstanceUpdate, _>(
//                         &client,
//                         backboard.clone(),
//                         mutations::service_instance_update::Variables {
//                             service_id,
//                             environment_id: env_id,
//                             source: mutations::service_instance_update::ServiceSourceInput {
//                                 image: None,
//                                 repo: Some(repository),
//                             },
//                         },
//                     )
//                     .await?;
//                 }
//                 Source::Docker(image) => {
//                     if let Some(id) = trigger {
//                         // delete old trigger
//                         post_graphql::<mutations::DeploymentTriggerDelete, _>(
//                             &client,
//                             backboard.clone(),
//                             mutations::deployment_trigger_delete::Variables { id },
//                         )
//                         .await?;
//                     }
//                     // update service information to use new source
//                     post_graphql::<mutations::ServiceInstanceUpdate, _>(
//                         &client,
//                         backboard.clone(),
//                         mutations::service_instance_update::Variables {
//                             service_id,
//                             environment_id: env_id,
//                             source: mutations::service_instance_update::ServiceSourceInput {
//                                 image: Some(image),
//                                 repo: None,
//                             },
//                         },
//                     )
//                     .await?;
//                 }
//             }
//             Ok(())
//         }));
//     }
//     let spinner = indicatif::ProgressBar::new_spinner()
//         .with_style(
//             indicatif::ProgressStyle::default_spinner()
//                 .tick_chars(TICK_STRING)
//                 .template("{spinner:.green} {msg}")?,
//         )
//         .with_message("Updating sources...");
//     spinner.enable_steady_tick(Duration::from_millis(100));
//     let r = futures::future::join_all(tasks).await;
//     for r in r {
//         r??;
//     }
//     spinner.finish_and_clear();
//     Ok(())
// }

#[derive(Clone, Debug, EnumDiscriminants, DeriveDisplay)]
#[strum_discriminants(derive(Display, EnumIter, EnumString), name(SourceTypes))]
pub enum Source {
    #[strum_discriminants(strum(
        to_string = "Docker image",
        serialize = "docker",
        serialize = "image"
    ))]
    #[display("Docker: {}", _0)]
    Docker(String),

    #[strum_discriminants(strum(
        to_string = "GitHub repo",
        serialize = "github",
        serialize = "git",
        serialize = "gh"
    ))]
    #[display("GitHub: {}/{}/{}", owner, repo, branch)]
    GitHub {
        owner: String,
        repo: String,
        branch: String,
    },
}

fn parse_repo(repo: String) -> Result<Source> {
    let s = repo
        .splitn(3, '/')
        .filter(|&s| !s.is_empty())
        .map(|s| s.to_string())
        .collect::<Vec<String>>();
    match s.len() {
        3 => Ok(Source::GitHub {
            owner: s.first().unwrap().to_string(),
            repo: s.get(1).unwrap().to_string(),
            branch: s.get(2).unwrap().to_string(),
        }),
        _ => anyhow::bail!("malformed repo: <owner>/<repo>/<branch>"),
    }
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

impl ChangeHandler for Variable {
    fn get_args(args: &NewArgs) -> &Vec<String> {
        &args.service_variables
    }

    fn parse_non_interactive(args: Vec<String>) -> Vec<(String, Variable)> {
        args.chunks(2)
            .filter_map(|chunk| {
                // clap ensures that there will always be 2 values whenever the flag is provided
                // this is unfiltered user input. validation of the service happens in the edit_services_select function
                let service = chunk.first()?.to_owned();

                let variable = match chunk.last()?.parse::<Variable>() {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e:?} (skipping)");
                        return None;
                    }
                };

                Some((service, variable))
            })
            .collect()
    }

    fn parse_interactive(service_name: &str) -> Result<Vec<Variable>> {
        prompt_variables(Some(service_name))
    }

    fn into_change(self) -> Change {
        Change::Variable(self)
    }
}

impl ChangeHandler for Source {
    fn get_args(args: &NewArgs) -> &Vec<String> {
        &args.service_sources
    }

    fn parse_non_interactive(args: Vec<String>) -> Vec<(String, Source)> {
        args.chunks(3)
            .filter_map(|chunk| {
                // clap ensures that there will always be 3 values whenever the flag is provided
                let service = chunk.first()?.to_owned();

                let source_type = match SourceTypes::from_str(&chunk.get(1)?.to_lowercase()) {
                    Ok(f) => {
                        fake_select("What do you want to change the source to?", &f.to_string());
                        f
                    }
                    Err(_) => {
                        eprintln!(
                            "Invalid platform. Valid platforms are: {} (skipping)",
                            SourceTypes::iter()
                                .map(|f| f.to_string())
                                .collect::<Vec<String>>()
                                .join(", ")
                        );
                        return None;
                    }
                };

                let source = match source_type {
                    SourceTypes::Docker => {
                        fake_select("Enter a docker image", chunk.last()?);
                        Some(Source::Docker(chunk.last()?.to_string()))
                    }
                    SourceTypes::GitHub => match parse_repo(chunk.last()?.to_string()) {
                        Ok(source) => Some(source),
                        Err(e) => {
                            eprintln!("{:?} (skipping)", e);
                            return None;
                        }
                    },
                }?;

                Some((service, source))
            })
            .collect()
    }

    fn parse_interactive(service_name: &str) -> Result<Vec<Source>> {
        let source_type = prompt_options(
            &format!("What type of source for {}?", service_name),
            SourceTypes::iter().collect(),
        )?;

        let source = match source_type {
            SourceTypes::Docker => {
                let image = prompt_text("Enter docker image")?;
                Source::Docker(image)
            }
            SourceTypes::GitHub => {
                let repo = prompt_text("Enter repo (owner/repo/branch)")?;
                parse_repo(repo)?
            }
        };

        Ok(vec![source])
    }

    fn into_change(self) -> Change {
        Change::Source(self)
    }
}
