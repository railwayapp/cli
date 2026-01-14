use anyhow::bail;
use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::{
    controllers::{
        project::{ensure_project_and_environment_exist, get_project},
        variables::Variable,
        workflow::wait_for_workflow,
    },
    util::{progress::create_spinner_if, prompt::prompt_text},
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
    variable: Vec<Variable>,
}

pub async fn command(args: Args) -> Result<()> {
    let mut configs = Configs::new()?;

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
        .into_iter()
        .map(|v| (v.key, v.value))
        .collect();

    for template in templates {
        if std::io::stdout().is_terminal() {
            fetch_and_create(
                &client,
                &mut configs,
                template.clone(),
                &linked_project,
                &variables,
                false,
                false,
                FetchAndCreateOptions::default(),
            )
            .await?;
        } else {
            println!("Creating {template}...");
            fetch_and_create(
                &client,
                &mut configs,
                template,
                &linked_project,
                &variables,
                false,
                false,
                FetchAndCreateOptions::default(),
            )
            .await?;
        }
    }

    Ok(())
}

/// Options for fetch_and_create
#[derive(Default)]
pub struct FetchAndCreateOptions {
    pub detach: bool,
    pub should_link: bool,
}

/// fetch database details via `TemplateDetail`
/// create database via `TemplateDeploy`
/// optionally wait for completion and link the new service
#[allow(clippy::too_many_arguments)]
pub async fn fetch_and_create(
    client: &reqwest::Client,
    configs: &mut Configs,
    template: String,
    linked_project: &LinkedProject,
    vars: &HashMap<String, String>,
    verbose: bool,
    json: bool,
    options: FetchAndCreateOptions,
) -> Result<(), anyhow::Error> {
    if verbose {
        println!("fetching details for template")
    }
    let details = post_graphql::<queries::TemplateDetail, _>(
        client,
        configs.get_backboard(),
        queries::template_detail::Variables {
            code: template.clone(),
        },
    )
    .await?;

    let template_name = details.template.name.clone();

    let mut config = DeserializedTemplateConfig::deserialize(
        &details.template.serialized_config.unwrap_or_default(),
    )?;

    ensure_project_and_environment_exist(client, configs, linked_project).await?;
    if verbose {
        println!("Project and environment in config exist");
    }

    // Get current services before the mutation
    let old_service_ids: HashSet<String> = {
        let project = get_project(client, configs, linked_project.project.clone()).await?;
        project
            .services
            .edges
            .iter()
            .map(|s| s.node.id.clone())
            .collect()
    };

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
                    variable
                        .description
                        .as_deref()
                        .map(|d| format!("   *{d}*\n"))
                        .unwrap_or_default(),
                ))?
            } else {
                continue;
            };

            variable.value = Some(value);
        }
    }

    let spinner = create_spinner_if(!json, format!("Adding {template_name}..."));

    let mutation_vars = mutations::template_deploy::Variables {
        project_id: linked_project.project.clone(),
        environment_id: linked_project.environment.clone(),
        template_id: details.template.id.clone(),
        serialized_config: serde_json::to_value(&config).context("Failed to serialize config")?,
    };
    if verbose {
        println!("deploying template");
    }
    let response = post_graphql::<mutations::TemplateDeploy, _>(
        client,
        configs.get_backboard(),
        mutation_vars,
    )
    .await?;

    // Wait for workflow to complete (unless detached)
    if !options.detach {
        if let Some(workflow_id) = response.template_deploy_v2.workflow_id {
            if verbose {
                println!("waiting for workflow {workflow_id} to complete");
            }
            wait_for_workflow(client, configs, workflow_id, &template_name).await?;
        }
    }

    // Find the newly created service
    let updated_project = get_project(client, configs, linked_project.project.clone()).await?;
    let new_service = updated_project
        .services
        .edges
        .iter()
        .find(|s| !old_service_ids.contains(&s.node.id));

    // Auto-link if should_link is true and no service is currently linked
    if options.should_link && linked_project.service.is_none() {
        if let Some(service) = new_service {
            configs.link_service(service.node.id.clone())?;
            configs.write()?;
            if verbose {
                println!("linked to service {}", service.node.name);
            }
        }
    }

    if json {
        let output = if let Some(service) = new_service {
            serde_json::json!({
                "templateId": details.template.id,
                "templateName": details.template.name,
                "serviceId": service.node.id,
                "serviceName": service.node.name,
            })
        } else {
            serde_json::json!({
                "templateId": details.template.id,
                "templateName": details.template.name,
            })
        };
        println!("{}", output);
    } else if let Some(spinner) = spinner {
        let mut msg = format!("🎉 Added {} to project", template_name.green().bold());
        if options.should_link && linked_project.service.is_none() && new_service.is_some() {
            msg.push_str(" and linked");
        }
        spinner.finish_with_message(msg);
    }
    if verbose {
        println!("template deployed");
    }

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
