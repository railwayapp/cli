use anyhow::bail;
use is_terminal::IsTerminal;
use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};
use strum::{Display, EnumIs, EnumIter, IntoEnumIterator};

use crate::{
    consts::TICK_STRING,
    controllers::{database::DatabaseType, project::ensure_project_and_environment_exist},
    util::prompt::{
        fake_select, prompt_multi_options, prompt_options, prompt_text,
        prompt_text_with_placeholder_disappear, prompt_text_with_placeholder_if_blank,
    },
};

use super::*;

/// Add a service to your project
#[derive(Parser)]
pub struct Args {
    /// The name of the database to add
    #[arg(short, long, value_enum)]
    database: Vec<DatabaseType>,

    /// The name of the service to create (leave blank for randomly generated)
    #[clap(short, long)]
    service: Option<Option<String>>,

    /// The repo to link to the service
    #[clap(short, long)]
    repo: Option<String>,

    /// The docker image to link to the service
    #[clap(short, long)]
    image: Option<String>,

    /// The "{key}={value}" environment variable pair to set the service variables.
    /// Example:
    ///
    /// railway add --service --variables "MY_SPECIAL_ENV_VAR=1" --variables "BACKEND_PORT=3000"
    #[clap(short, long)]
    variables: Vec<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;
    let type_of_create = if !args.database.is_empty() {
        fake_select("What do you need?", "Database");
        CreateKind::Database(args.database)
    } else if args.repo.is_some() {
        fake_select("What do you need?", "GitHub Repo");
        CreateKind::GithubRepo {
            repo: prompt_repo(args.repo)?,
            variables: prompt_variables(args.variables)?,
            name: prompt_name(args.service)?,
        }
    } else if args.image.is_some() {
        fake_select("What do you need?", "Docker Image");
        CreateKind::DockerImage {
            image: prompt_image(args.image)?,
            variables: prompt_variables(args.variables)?,
            name: prompt_name(args.service)?,
        }
    } else if args.service.is_some() {
        fake_select("What do you need?", "Empty Service");
        CreateKind::EmptyService {
            name: prompt_name(args.service)?,
            variables: prompt_variables(args.variables)?,
        }
    } else {
        let need = prompt_options("What do you need?", CreateKind::iter().collect())?;
        match need {
            CreateKind::Database(_) => CreateKind::Database(prompt_database()?),
            CreateKind::EmptyService { .. } => CreateKind::EmptyService {
                name: prompt_name(args.service)?,
                variables: prompt_variables(args.variables)?,
            },
            CreateKind::GithubRepo { .. } => {
                let repo = prompt_repo(args.repo)?;
                let variables = prompt_variables(args.variables)?;
                CreateKind::GithubRepo {
                    repo,
                    variables,
                    name: prompt_name(args.service)?,
                }
            }
            CreateKind::DockerImage { .. } => {
                let image = prompt_image(args.image)?;
                let variables = prompt_variables(args.variables)?;
                CreateKind::DockerImage {
                    image,
                    variables,
                    name: prompt_name(args.service)?,
                }
            }
        }
    };

    match type_of_create {
        CreateKind::Database(databases) => {
            for db in databases {
                deploy::fetch_and_create(
                    &client,
                    &configs,
                    db.to_slug().to_string(),
                    &linked_project,
                    &HashMap::new(),
                )
                .await?;
            }
        }
        CreateKind::DockerImage {
            image,
            variables,
            name,
        } => {
            create_service(
                name,
                &linked_project,
                &client,
                &mut configs,
                None,
                Some(image),
                variables,
            )
            .await?;
        }
        CreateKind::GithubRepo {
            repo,
            variables,
            name,
        } => {
            create_service(
                name,
                &linked_project,
                &client,
                &mut configs,
                Some(repo),
                None,
                variables,
            )
            .await?;
        }
        CreateKind::EmptyService { name, variables } => {
            create_service(
                name,
                &linked_project,
                &client,
                &mut configs,
                None,
                None,
                variables,
            )
            .await?;
        }
    }
    Ok(())
}

fn prompt_database() -> Result<Vec<DatabaseType>, anyhow::Error> {
    if !std::io::stdout().is_terminal() {
        bail!("No database specified");
    }
    prompt_multi_options("Select databases to add", DatabaseType::iter().collect())
}

fn prompt_repo(repo: Option<String>) -> Result<String> {
    if let Some(repo) = repo {
        fake_select("Enter a repo", &repo);
        return Ok(repo);
    }

    prompt_text_with_placeholder_disappear("Enter a repo", "<user/org>/<repo name>")
}

fn prompt_image(image: Option<String>) -> Result<String> {
    if let Some(image) = image {
        fake_select("Enter an image", &image);
        return Ok(image);
    }
    prompt_text("Enter an image")
}

fn prompt_name(service: Option<Option<String>>) -> Result<Option<String>> {
    if let Some(name) = service {
        if let Some(name) = name {
            fake_select("Enter a service name", &name);
            Ok(Some(name))
        } else {
            fake_select("Enter a service name", "<randomly generated>");
            Ok(None)
        }
    } else if std::io::stdout().is_terminal() {
        return Ok(Some(prompt_text_with_placeholder_if_blank(
            "Enter a service name",
            "<leave blank for randomly generated>",
            "<randomly generated>",
        )?)
        .filter(|s| !s.trim().is_empty()));
    } else {
        fake_select("Enter a service name", "<randomly generated>");
        Ok(None)
    }
}

fn prompt_variables(variables: Vec<String>) -> Result<Option<BTreeMap<String, String>>> {
    if !std::io::stdout().is_terminal() && variables.is_empty() {
        fake_select("Enter a variable", "");
        return Ok(None);
    }
    if variables.is_empty() {
        let mut variables = BTreeMap::<String, String>::new();
        loop {
            let v = prompt_text_with_placeholder_disappear(
                "Enter a variable",
                "<KEY=VALUE, press enter to skip>",
            )?;
            if v.trim().is_empty() {
                break;
            }
            let mut split = v.split('=').peekable();
            if split.peek().is_none() {
                continue;
            }
            let key = split.next().unwrap().trim().to_owned();
            if split.peek().is_none() {
                continue;
            }
            let value = split.collect::<Vec<&str>>().join("=").trim().to_owned();
            variables.insert(key, value);
        }
        return Ok(if variables.is_empty() {
            None
        } else {
            Some(variables)
        });
    }
    let variables: BTreeMap<String, String> = variables
        .iter()
        .filter_map(|v| {
            let mut split = v.split('=');
            let key = split.next()?.trim().to_owned();
            let value = split.collect::<Vec<&str>>().join("=").trim().to_owned();
            if value.is_empty() {
                None
            } else {
                fake_select("Enter a variable", &format!("{}={}", key, value));
                Some((key, value))
            }
        })
        .collect();
    Ok(Some(variables))
}

type Variables = Option<BTreeMap<String, String>>;

async fn create_service(
    service: Option<String>,
    linked_project: &LinkedProject,
    client: &reqwest::Client,
    configs: &mut Configs,
    repo: Option<String>,
    image: Option<String>,
    variables: Variables,
) -> Result<(), anyhow::Error> {
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message("Creating service...");
    spinner.enable_steady_tick(Duration::from_millis(100));
    let source = mutations::service_create::ServiceSourceInput { repo, image };
    let branch = if let Some(repo) = &source.repo {
        let repos = post_graphql::<queries::GitHubRepos, _>(
            client,
            &configs.get_backboard(),
            queries::git_hub_repos::Variables {},
        )
        .await?
        .github_repos;
        let repo = repos
            .iter()
            .find(|r| r.full_name == *repo)
            .ok_or(anyhow::anyhow!("repo not found"))?;
        Some(repo.default_branch.clone())
    } else {
        None
    };
    let vars = mutations::service_create::Variables {
        name: service,
        project_id: linked_project.project.clone(),
        environment_id: linked_project.environment.clone(),
        source: Some(source),
        variables,
        branch,
    };
    let s =
        post_graphql::<mutations::ServiceCreate, _>(client, &configs.get_backboard(), vars).await?;
    configs.link_service(s.service_create.id)?;
    configs.write()?;
    spinner.finish_with_message(format!(
        "Succesfully created the service \"{}\" and linked to it",
        s.service_create.name.blue()
    ));
    Ok(())
}

#[derive(Debug, Clone, EnumIter, Display, EnumIs)]
enum CreateKind {
    #[strum(to_string = "GitHub Repo")]
    GithubRepo {
        repo: String,
        variables: Variables,
        name: Option<String>,
    },
    #[strum(to_string = "Database")]
    Database(Vec<DatabaseType>),
    #[strum(to_string = "Docker Image")]
    DockerImage {
        image: String,
        variables: Variables,
        name: Option<String>,
    },
    #[strum(to_string = "Empty Service")]
    EmptyService {
        name: Option<String>,
        variables: Variables,
    },
}
