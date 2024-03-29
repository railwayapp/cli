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

/// Add a new plugin to your project
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
            bail!("No plugins specified");
        }
        prompt_multi_options("Select databases to add", DatabaseType::iter().collect())?
    } else {
        args.database
    };

    if databases.is_empty() {
        bail!("No plugins selected");
    }

    for db in databases {
        // fetch template detail
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
            let mut s_var = BTreeMap::<String, String>::new();
            let mut s_vol = Vec::<TemplateVolume>::new();
            // all variables in a db template have default values, this is safe
            for variable in s.node.config.variables.clone() {
                s_var.insert(
                    variable.name.clone(),
                    variable.default_value.clone().unwrap(),
                );
            }
            if let Some(volumes) = s.node.config.volumes.clone() {
                for volume in volumes {
                    s_vol.push(TemplateVolume {
                        mount_path: volume.mount_path.clone(),
                        name: volume.name.clone(),
                    });
                }
            }

            mutations::template_deploy::TemplateDeployService {
                commit: None,
                has_domain: Some(
                    s.node
                        .config
                        .domains
                        .clone()
                        .iter()
                        .find(|d| d.has_service_domain)
                        .map(|s| s.has_service_domain)
                        .unwrap_or(false),
                ),
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
                    .clone()
                    .and_then(|deploy_config| deploy_config.start_command),
                tcp_proxy_application_port: s.node.config.tcp_proxies.clone().and_then(
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
