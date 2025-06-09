use crate::queries::project::DeploymentStatus;

use super::*;
use chrono_humanize::HumanTime;
use queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges};
use std::fmt::Write as _;

pub async fn list(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
) -> Result<()> {
    // first get all services
    // then filter through all services that have a service instance in the specified environment
    // then check if the image is the bun runtime image
    let functions: Vec<&queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges> =
        project
            .services
            .edges
            .iter()
            .filter_map(|s| {
                s.node
                    .service_instances
                    .edges
                    .iter()
                    .find(|e| e.node.environment_id == environment.node.id)
            })
            .filter(|s| {
                s.node.source.clone().is_some_and(|s| {
                    s.image
                        .unwrap_or_default()
                        .starts_with("ghcr.io/railwayapp/function") // there is only one runtime right now, in the format function-RUNTIME
                })
            })
            .collect();
    if functions.is_empty() {
        println!(
            "No functions in project {} and environment {}",
            project.name.magenta(),
            environment.node.name.magenta()
        );
        return Ok(());
    }

    let info = functions
        .iter()
        .map(|f| {
            let mut n = String::new();
            let coloured_name = if let Some(ref deployment) = f.node.latest_deployment {
                match deployment.status {
                    DeploymentStatus::BUILDING
                    | DeploymentStatus::DEPLOYING
                    | DeploymentStatus::INITIALIZING
                    | DeploymentStatus::QUEUED => f.node.service_name.blue(),
                    DeploymentStatus::CRASHED | DeploymentStatus::FAILED => {
                        f.node.service_name.red()
                    }
                    DeploymentStatus::SLEEPING => f.node.service_name.yellow(),
                    DeploymentStatus::SUCCESS => f.node.service_name.green(),
                    _ => f.node.service_name.dimmed(),
                }
            } else {
                f.node.service_name.dimmed()
            };
            write!(n, "{}", coloured_name).unwrap();
            // get runtime and version
            if let Some(ref source) = f.node.source {
                if let Some(image) = &source.image {
                    // function-RUNTIME:version of runtime
                    let runtime_unparsed = image.split("function-").nth(1).unwrap();
                    let mut runtime = runtime_unparsed.split(":");
                    write!(
                        n,
                        " ({} {}{})",
                        runtime.next().unwrap().blue(),
                        "v".purple(),
                        runtime.next().unwrap().purple()
                    )
                    .unwrap();
                }
            }
            if let Some(next_run) = f.node.next_cron_run_at {
                let ht = HumanTime::from(next_run);
                write!(n, " (next run {})", ht.to_string().yellow()).unwrap();
            }
            if !f.node.domains.custom_domains.is_empty()
                || !f.node.domains.service_domains.is_empty()
            {
                write!(n, " ({})", "http".blue()).unwrap();
            }
            n
        })
        .collect::<Vec<String>>()
        .join("\n");
    println!(
        "Functions in project {} and environment {}:\n{}",
        project.name.magenta(),
        environment.node.name.magenta(),
        info
    );
    Ok(())
}
