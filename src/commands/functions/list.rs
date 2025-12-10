use crate::queries::project::DeploymentStatus;

use super::*;
use chrono_humanize::HumanTime;
use pathdiff::diff_paths;
use queries::project::{
    ProjectProject, ProjectProjectEnvironmentsEdges,
    ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
};
use std::{fmt::Write as _, path::Path};

pub async fn list(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
) -> Result<()> {
    let functions = common::get_functions_in_environment(environment);
    if functions.is_empty() {
        display_no_functions_message(&project, environment);
        return Ok(());
    }

    let function_info = format_functions_list(&functions)?;
    display_functions_list(&project, environment, &function_info);

    Ok(())
}

fn format_functions_list(
    functions: &[&ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges],
) -> Result<String> {
    let configs = Configs::new()?;
    let closest = configs.get_functions_in_directory(
        Path::new(&configs.get_closest_linked_project_directory()?).to_path_buf(),
    )?;
    functions
        .iter()
        .try_fold(String::new(), |mut acc, function| {
            if !acc.is_empty() {
                acc.push('\n');
            }
            acc.push_str(&format_function_entry(function, closest.clone())?);
            Ok::<String, anyhow::Error>(acc)
        })
}

fn format_function_entry(
    function: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
    closest: Vec<(PathBuf, String)>,
) -> Result<String> {
    let mut entry = String::new();

    let colored_name = get_colored_function_name(function);
    write!(entry, "{colored_name}")?;

    append_runtime_info(&mut entry, function)?;
    append_next_cron_run(&mut entry, function)?;
    append_domain_info(&mut entry, function)?;
    append_linked_information(&mut entry, function, closest)?;

    Ok(entry)
}

fn get_colored_function_name(
    function: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
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
    function: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
) -> Result<()> {
    if let Some(ref source) = function.node.source {
        if let Some(image) = &source.image {
            if let Some(runtime_info) = parse_runtime_from_image(image) {
                write!(
                    entry,
                    " ({} {}{})",
                    runtime_info.0.blue(),
                    "v".purple(),
                    runtime_info.1.purple()
                )?;
            }
        }
    }
    Ok(())
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
    function: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
) -> Result<()> {
    if let Some(next_run) = function.node.next_cron_run_at {
        let human_time = HumanTime::from(next_run);
        write!(entry, " (next run {})", human_time.to_string().yellow())?;
    }
    Ok(())
}

fn append_domain_info(
    entry: &mut String,
    function: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
) -> Result<()> {
    if common::has_domains(function) {
        write!(entry, " ({})", "http".blue())?;
    }
    Ok(())
}

fn append_linked_information(
    entry: &mut String,
    function: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
    closest: Vec<(PathBuf, String)>,
) -> Result<()> {
    if !closest.is_empty() {
        if let Some((path, _)) = closest
            .iter()
            .find(|(_, id)| *id == function.node.service_id)
        {
            let p = if let Some(diffed) = diff_paths(path, std::env::current_dir()?) {
                diffed.display().to_string()
            } else {
                path.display().to_string()
            };
            write!(entry, " ({})", p.green())?;
        }
    }
    Ok(())
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
