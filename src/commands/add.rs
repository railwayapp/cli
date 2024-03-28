use anyhow::bail;
use is_terminal::IsTerminal;
use std::collections::BTreeMap;
use std::time::Duration;
use strum::IntoEnumIterator;

use crate::{
    consts::TICK_STRING,
    controllers::{database::DatabaseType, project::get_project},
    mutations::TemplateVolume,
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

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

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
        let details = post_graphql::<queries::TemplateDetail, _>(
            &client,
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
                s.node.config.variables.iter().for_each(|v| {
                    s_var.insert(v.name.clone(), v.default_value.clone().unwrap());
                });
                if let Some(volumes) = s.node.config.volumes.clone() {
                    volumes.iter().for_each(|v| {
                        s_vol.push(TemplateVolume {
                            mount_path: v.mount_path.clone(),
                            name: v.name.clone(),
                        })
                    })
                }
                impl std::fmt::Debug for mutations::template_deploy::TemplateDeployService {
                    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.debug_struct("TemplateDeployService")
                            .field("commit", &self.commit)
                            .field("has_domain", &self.has_domain)
                            .field("healthcheck_path", &self.healthcheck_path)
                            .field("id", &self.id)
                            .field("is_private", &self.is_private)
                            .field("name", &self.name)
                            .field("owner", &self.owner)
                            .field("root_directory", &self.root_directory)
                            .field("service_icon", &self.service_icon)
                            .field("service_name", &self.service_name)
                            .field("start_command", &self.start_command)
                            .field(
                                "tcp_proxy_application_port",
                                &self.tcp_proxy_application_port,
                            )
                            .field("template", &self.template)
                            .field("variables", &self.variables)
                            .field("volumes", &self.volumes)
                            .finish()
                    }
                }
                mutations::template_deploy::TemplateDeployService {
                    commit: None,
                    has_domain: s
                        .node
                        .config
                        .domains
                        .clone()
                        .iter()
                        .find(|d| d.has_service_domain)
                        .map(|s| s.has_service_domain),
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
        dbg!(services.iter().for_each(|s| {
            dbg!(s);
        }));

        let vars = mutations::template_deploy::Variables {
            project_id: linked_project.project.clone(),
            environment_id: linked_project.environment.clone(),
            services,
            template_code: db.to_slug(),
        };
        let created_plugin =
            post_graphql::<mutations::TemplateDeploy, _>(&client, configs.get_backboard(), vars)
                .await?;
        dbg!(created_plugin);
    }

    /*
        pub struct TemplateDeployService {
        pub commit: Option<String>,
        #[serde(rename = "hasDomain")]
        pub has_domain: Option<Boolean>,
        #[serde(rename = "healthcheckPath")]
        pub healthcheck_path: Option<String>,
        pub id: Option<String>,
        #[serde(rename = "isPrivate")]
        pub is_private: Option<Boolean>,
        pub name: Option<String>,
        pub owner: Option<String>,
        #[serde(rename = "rootDirectory")]
        pub root_directory: Option<String>,
        #[serde(rename = "serviceIcon")]
        pub service_icon: Option<String>,
        #[serde(rename = "serviceName")]
        pub service_name: String,
        #[serde(rename = "startCommand")]
        pub start_command: Option<String>,
        #[serde(rename = "tcpProxyApplicationPort")]
        pub tcp_proxy_application_port: Option<Int>,
        pub template: String,
        pub variables: Option<ServiceVariables>,
        pub volumes: Option<Vec<TemplateVolume>>,
    }

     */

    // for plugin in selected {
    //     let vars = mutations::plugin_create::Variables {
    //         project_id: linked_project.project.clone(),
    //         name: plugin.to_lowercase(),
    //     };
    //     if std::io::stdout().is_terminal() {
    //         let spinner = indicatif::ProgressBar::new_spinner()
    //             .with_style(
    //                 indicatif::ProgressStyle::default_spinner()
    //                     .tick_chars(TICK_STRING)
    //                     .template("{spinner:.green} {msg}")?,
    //             )
    //             .with_message(format!("Creating {plugin}..."));
    //         spinner.enable_steady_tick(Duration::from_millis(100));
    //         post_graphql::<mutations::PluginCreate, _>(&client, configs.get_backboard(), vars)
    //             .await?;
    //         spinner.finish_with_message(format!("Created {plugin}"));
    //     } else {
    //         println!("Creating {}...", plugin);
    //         post_graphql::<mutations::PluginCreate, _>(&client, configs.get_backboard(), vars)
    //             .await?;
    //     }
    // }

    Ok(())
}
