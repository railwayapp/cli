use anyhow::bail;
use is_terminal::IsTerminal;
use std::collections::{HashMap, HashSet};

use crate::{
    controllers::{
        project::{ensure_project_and_environment_exist, get_project},
        variables::Variable,
        workflow::{WorkflowError, wait_for_workflow},
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
        eprintln!("fetching details for template")
    }
    let public_client = GQLClient::new_public()?;
    let details = post_graphql::<queries::TemplateDetail, _>(
        &public_client,
        configs.get_backboard(),
        queries::template_detail::Variables {
            code: template.clone(),
        },
    )
    .await?;

    let template_name = details.template.name.clone();

    // Work on the raw JSON value: the config is passed through to the
    // deploy mutation verbatim (only variable values are filled in below).
    // Round-tripping it through typed structs silently dropped every field
    // the structs didn't declare — clusterRole, parentServiceId, exposure,
    // numReplicas, backup schedules, variable generators/sealing, … — so
    // templates deployed via the CLI lost their cluster metadata and
    // replica/volume configuration.
    let mut config = details.template.serialized_config.unwrap_or_default();

    ensure_project_and_environment_exist(client, configs, linked_project).await?;
    if verbose {
        eprintln!("Project and environment in config exist");
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

    let services = config
        .get_mut("services")
        .and_then(|s| s.as_object_mut())
        .map(|s| s.values_mut().collect::<Vec<_>>())
        .unwrap_or_default();
    for service in services {
        let service_name = service
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(variables) = service.get_mut("variables").and_then(|v| v.as_object_mut()) else {
            continue;
        };
        for (key, variable) in variables.iter_mut() {
            if variable.is_null() {
                continue;
            }
            let default_value = variable
                .get("defaultValue")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty());
            let is_optional = variable
                .get("isOptional")
                .and_then(|v| v.as_bool())
                .unwrap_or_default();

            let value = if let Some(value) = vars.get(&format!("{service_name}.{key}")) {
                value.clone()
            } else if let Some(value) = vars.get(key) {
                value.clone()
            } else if let Some(value) = default_value {
                value.to_string()
            } else if !is_optional {
                let description = variable
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|d| format!("   *{d}*\n"))
                    .unwrap_or_default();
                prompt_text(&format!(
                    "Environment Variable {key} for service {service_name} is required, please set a value:\n{description}",
                ))?
            } else {
                continue;
            };

            if let Some(variable) = variable.as_object_mut() {
                variable.insert("value".to_string(), serde_json::Value::String(value));
            }
        }
    }

    let spinner = create_spinner_if(!json, format!("Adding {template_name}..."));

    let mutation_vars = mutations::template_deploy::Variables {
        project_id: linked_project.project.clone(),
        environment_id: linked_project.environment_id()?.to_string(),
        template_id: details.template.id.clone(),
        serialized_config: config,
    };
    if verbose {
        eprintln!("deploying template");
    }
    let response = post_graphql::<mutations::TemplateDeploy, _>(
        client,
        configs.get_backboard(),
        mutation_vars,
    )
    .await?;

    // Wait for workflow to complete
    if let Some(workflow_id) = response.template_deploy_v2.workflow_id {
        if verbose {
            eprintln!("waiting for workflow {workflow_id} to complete");
        }
        wait_for_workflow(client, configs, workflow_id)
            .await
            .map_err(|e| match e {
                WorkflowError::Failed(msg) => {
                    anyhow::anyhow!("Failed to add {template_name}: {msg}")
                }
                WorkflowError::NotFound => anyhow::anyhow!("Failed to add {template_name}"),
                WorkflowError::Timeout => {
                    anyhow::anyhow!("Timed out waiting for {template_name} to finish deploying")
                }
            })?;
    }

    // Find the newly created service
    let updated_project = get_project(client, configs, linked_project.project.clone()).await?;
    let new_service = updated_project
        .services
        .edges
        .iter()
        .find(|s| !old_service_ids.contains(&s.node.id));

    // Env-var / token targeted runs have no on-disk link entry to update;
    // linking there would fail with ProjectNotFound after the template was
    // already deployed.
    let should_auto_link = options.should_link
        && linked_project.service.is_none()
        && !Configs::uses_env_project_targeting();
    if should_auto_link {
        if let Some(service) = new_service {
            configs.link_service(service.node.id.clone())?;
            configs.write()?;
            if verbose {
                eprintln!("linked to service {}", service.node.name);
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
        if should_auto_link && new_service.is_some() {
            msg.push_str(" and linked");
        }
        spinner.finish_with_message(msg);
    }
    if verbose {
        eprintln!("template deployed");
    }

    Ok(())
}
