use crate::queries::project::DeploymentStatus;

use super::*;
use chrono_humanize::HumanTime;
use queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges};
use std::fmt::Write as _;

pub async fn list(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
) -> Result<()> {
    let functions = get_functions_in_environment(&project, environment);

    if functions.is_empty() {
        display_no_functions_message(&project, environment);
        return Ok(());
    }

    let function_info = format_functions_list(&functions);
    display_functions_list(&project, environment, &function_info);

    Ok(())
}

fn get_functions_in_environment<'a>(
    project: &'a ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
) -> Vec<&'a queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges> {
    project
        .services
        .edges
        .iter()
        .filter_map(|service| {
            service
                .node
                .service_instances
                .edges
                .iter()
                .find(|instance| instance.node.environment_id == environment.node.id)
        })
        .filter(|service_instance| is_function_service(service_instance))
        .collect()
}

fn is_function_service(
    service_instance: &queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges,
) -> bool {
    service_instance.node.source.clone().is_some_and(|source| {
        source
            .image
            .unwrap_or_default()
            .starts_with("ghcr.io/railwayapp/function")
    })
}

fn format_functions_list(
    functions: &[&queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges],
) -> String {
    functions
        .iter()
        .map(|function| format_function_entry(function))
        .collect::<Vec<String>>()
        .join("\n")
}

fn format_function_entry(
    function: &queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges,
) -> String {
    let mut entry = String::new();

    let colored_name = get_colored_function_name(function);
    write!(entry, "{}", colored_name).unwrap();

    append_runtime_info(&mut entry, function);
    append_next_cron_run(&mut entry, function);
    append_domain_info(&mut entry, function);

    entry
}

fn get_colored_function_name(
    function: &queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges,
) -> colored::ColoredString {
    if let Some(ref deployment) = function.node.latest_deployment {
        match deployment.status {
            DeploymentStatus::BUILDING
            | DeploymentStatus::DEPLOYING
            | DeploymentStatus::INITIALIZING
            | DeploymentStatus::QUEUED => function.node.service_name.blue(),
            DeploymentStatus::CRASHED | DeploymentStatus::FAILED => {
                function.node.service_name.red()
            }
            DeploymentStatus::SLEEPING => function.node.service_name.yellow(),
            DeploymentStatus::SUCCESS => function.node.service_name.green(),
            _ => function.node.service_name.dimmed(),
        }
    } else {
        function.node.service_name.dimmed()
    }
}

fn append_runtime_info(
    entry: &mut String,
    function: &queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges,
) {
    if let Some(ref source) = function.node.source {
        if let Some(image) = &source.image {
            if let Some(runtime_info) = parse_runtime_from_image(image) {
                write!(
                    entry,
                    " ({} {}{})",
                    runtime_info.0.blue(),
                    "v".purple(),
                    runtime_info.1.purple()
                )
                .unwrap();
            }
        }
    }
}

fn parse_runtime_from_image(image: &str) -> Option<(String, String)> {
    let runtime_unparsed = image.split("function-").nth(1)?;
    let mut runtime_parts = runtime_unparsed.split(":");
    let runtime_name = runtime_parts.next()?.to_string();
    let runtime_version = runtime_parts.next()?.to_string();
    Some((runtime_name, runtime_version))
}

fn append_next_cron_run(
    entry: &mut String,
    function: &queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges,
) {
    if let Some(next_run) = function.node.next_cron_run_at {
        let human_time = HumanTime::from(next_run);
        write!(entry, " (next run {})", human_time.to_string().yellow()).unwrap();
    }
}

fn append_domain_info(
    entry: &mut String,
    function: &queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges,
) {
    if has_domains(function) {
        write!(entry, " ({})", "http".blue()).unwrap();
    }
}

fn has_domains(
    function: &queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges,
) -> bool {
    !function.node.domains.custom_domains.is_empty()
        || !function.node.domains.service_domains.is_empty()
}

fn display_no_functions_message(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
) {
    println!(
        "No functions in project {} and environment {}",
        project.name.magenta(),
        environment.node.name.magenta()
    );
}

fn display_functions_list(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    function_info: &str,
) {
    println!(
        "Functions in project {} and environment {}:\n{}",
        project.name.magenta(),
        environment.node.name.magenta(),
        function_info
    );
}
