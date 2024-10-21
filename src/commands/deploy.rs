use anyhow::bail;
use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use crate::{
    consts::TICK_STRING, controllers::project::ensure_project_and_environment_exist,
    util::prompt::prompt_text,
};

use super::*;

/// Provisions a template into your project
#[derive(Parser)]
pub struct Args {
    /// The code of the template to deploy
    #[arg(short, long)]
    template: Vec<String>,

    /// The "{key}={value}" environment variable pair to set the template variables
    ///
    /// To specify the variable for a single service prefix it with "{service}."
    /// Example:
    ///
    /// railway deploy -t postgres -v "MY_SPECIAL_ENV_VAR=1" -v "Backend.Port=3000"
    #[arg(short, long)]
    variable: Vec<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let templates = if args.template.is_empty() {
        if !std::io::stdout().is_terminal() {
            bail!("No template specified");
        }
        vec![prompt_text("Select template to deploy")?]
    } else {
        args.template
    };

    if templates.is_empty() {
        bail!("No template selected");
    }

    let variables: HashMap<String, String> = args
        .variable
        .iter()
        .map(|v| {
            let mut split = v.split('=');
            let key = split.next().unwrap_or_default().trim().to_owned();
            let value = split.collect::<Vec<&str>>().join("=").trim().to_owned();
            (key, value)
        })
        .filter(|(_, value)| !value.is_empty())
        .collect();

    for template in templates {
        if std::io::stdout().is_terminal() {
            fetch_and_create(
                &client,
                &configs,
                template.clone(),
                &linked_project,
                &variables,
            )
            .await?;
        } else {
            println!("Creating {}...", template);
            fetch_and_create(&client, &configs, template, &linked_project, &variables).await?;
        }
    }

    Ok(())
}

/// fetch database details via `TemplateDetail`
/// create database via `TemplateDeploy`
pub async fn fetch_and_create(
    client: &reqwest::Client,
    configs: &Configs,
    template: String,
    linked_project: &LinkedProject,
    vars: &HashMap<String, String>,
) -> Result<(), anyhow::Error> {
    let details = post_graphql::<queries::TemplateDetail, _>(
        client,
        configs.get_backboard(),
        queries::template_detail::Variables {
            code: template.clone(),
        },
    )
    .await?;

    let mut config = DeserializedTemplateConfig::deserialize(
        &details.template.serialized_config.unwrap_or_default(),
    )?;

    ensure_project_and_environment_exist(client, configs, linked_project).await?;

    for s in &mut config.services.values_mut() {
        for (key, variable) in &mut s.variables {
            let value = if let Some(value) = vars.get(&format!("{}.{key}", s.name)) {
                value.clone()
            } else if let Some(value) = vars.get(key) {
                value.clone()
            } else if let Some(value) = variable.default_value.as_ref().filter(|v| !v.is_empty()) {
                value.clone()
            } else if !variable.is_optional.unwrap_or_default() {
                prompt_text(&format!(
                    "Environment Variable {key} for service {} is required, please set a value:\n{}",
                    s.name,
                    variable.description.as_deref().map(|d| format!("   *{d}*\n")).unwrap_or_default(),
                ))?
            } else {
                continue;
            };

            variable.value = Some(value);
        }
    }

    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message(format!("Creating {template}..."));

    spinner.enable_steady_tick(Duration::from_millis(100));

    let vars = mutations::template_deploy::Variables {
        project_id: linked_project.project.clone(),
        environment_id: linked_project.environment.clone(),
        template_id: details.template.id,
        serialized_config: serde_json::to_value(&config).context("Failed to serialize config")?,
    };

    post_graphql::<mutations::TemplateDeploy, _>(client, configs.get_backboard(), vars).await?;

    spinner.finish_with_message(format!(
        "🎉 Added {} to project",
        details.template.name.green().bold(),
    ));

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedServiceNetworking {
    #[serde(default)]
    service_domains: HashMap<String, serde_json::Value>,

    #[serde(default)]
    tcp_proxies: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedServiceVolumeMount {
    mount_path: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedServiceVariable {
    #[serde(default)]
    default_value: Option<String>,

    #[serde(default)]
    value: Option<String>,

    #[serde(default)]
    description: Option<String>,

    #[serde(default)]
    is_optional: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedServiceDeploy {
    healthcheck_path: Option<String>,
    start_command: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum DeserializedServiceSource {
    Image {
        image: String,
    },
    #[serde(rename_all = "camelCase")]
    Repo {
        root_directory: Option<String>,
        repo: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedTemplateService {
    #[serde(default)]
    deploy: Option<DeserializedServiceDeploy>,

    #[serde(default)]
    icon: Option<String>,
    name: String,

    #[serde(default)]
    networking: Option<DeserializedServiceNetworking>,

    #[serde(default)]
    source: Option<DeserializedServiceSource>,

    #[serde(default)]
    variables: HashMap<String, DeserializedServiceVariable>,
    #[serde(default)]
    volume_mounts: HashMap<String, DeserializedServiceVolumeMount>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedTemplateConfig {
    #[serde(default)]
    services: HashMap<String, DeserializedTemplateService>,
}
