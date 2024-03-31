use anyhow::bail;
use is_terminal::IsTerminal;
use std::collections::BTreeMap;
use std::time::Duration;
use strum::IntoEnumIterator;

use crate::{
    consts::TICK_STRING, controllers::database::DatabaseType, mutations::TemplateVolume,
    util::prompt::prompt_multi_options,
};

use super::*;

/// Provision a database into your project
#[derive(Parser)]
pub struct Args {
    /// The name of the database to add
    #[arg(short, long, value_enum)]
    database: Vec<DatabaseType>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let databases = if args.database.is_empty() {
        if !std::io::stdout().is_terminal() {
            bail!("No database specified");
        }
        prompt_multi_options("Select databases to add", DatabaseType::iter().collect())?
    } else {
        args.database
    };

    if databases.is_empty() {
        bail!("No database selected");
    }

    for db in databases {
        if std::io::stdout().is_terminal() {
            let spinner = indicatif::ProgressBar::new_spinner()
                .with_style(
                    indicatif::ProgressStyle::default_spinner()
                        .tick_chars(TICK_STRING)
                        .template("{spinner:.green} {msg}")?,
                )
                .with_message(format!("Creating {db}..."));
            spinner.enable_steady_tick(Duration::from_millis(100));
            fetch_and_create(&client, &configs, db.clone(), &linked_project).await?;
            spinner.finish_with_message(format!("Created {db}"));
        } else {
            println!("Creating {}...", db);
            fetch_and_create(&client, &configs, db, &linked_project).await?;
        }
    }

    Ok(())
}
/// fetch database details via `TemplateDetail`
/// create database via `TemplateDeploy`
async fn fetch_and_create(
    client: &reqwest::Client,
    configs: &Configs,
    db: DatabaseType,
    linked_project: &LinkedProject,
) -> Result<(), anyhow::Error> {
    let details = post_graphql::<queries::TemplateDetail, _>(
        client,
        configs.get_backboard(),
        queries::template_detail::Variables { code: db.to_slug() },
    )
    .await?;

    let services: Vec<mutations::template_deploy::TemplateDeployService> = details
        .template
        .services
        .edges
        .iter()
        .map(|s| {
            let s_var = s
                .node
                .config
                .variables
                .iter()
                .map(|variable| {
                    (
                        variable.name.clone(),
                        variable.default_value.clone().unwrap(),
                    )
                })
                .collect::<BTreeMap<String, String>>();

            let s_vol = s
                .node
                .config
                .volumes
                .clone()
                .map(|volumes| {
                    volumes
                        .into_iter()
                        .map(|volume| TemplateVolume {
                            mount_path: volume.mount_path.clone(),
                            name: volume.name.clone(),
                        })
                        .collect::<Vec<TemplateVolume>>()
                })
                .unwrap_or_default();

            mutations::template_deploy::TemplateDeployService {
                commit: None,
                has_domain: Some(s.node.config.domains.iter().any(|d| d.has_service_domain)),
                healthcheck_path: None,
                id: Some(s.node.id.clone()),
                is_private: None,
                name: Some(s.node.config.name.clone()),
                owner: None,
                root_directory: None,
                service_icon: s.node.config.icon.clone(),
                service_name: s.node.config.name.clone(),
                start_command: s
                    .node
                    .config
                    .deploy_config
                    .as_ref()
                    .and_then(|deploy_config| deploy_config.start_command.clone()),
                tcp_proxy_application_port: s.node.config.tcp_proxies.as_ref().and_then(
                    |tcp_proxies| tcp_proxies.first().map(|first| first.application_port),
                ),
                template: s.node.config.source.image.clone(),
                variables: (!s_var.is_empty()).then_some(s_var),
                volumes: (!s_vol.is_empty()).then_some(s_vol),
            }
        })
        .collect();

    let vars = mutations::template_deploy::Variables {
        project_id: linked_project.project.clone(),
        environment_id: linked_project.environment.clone(),
        services,
        template_code: db.to_slug(),
    };

    post_graphql::<mutations::TemplateDeploy, _>(client, configs.get_backboard(), vars).await?;
    Ok(())
}
