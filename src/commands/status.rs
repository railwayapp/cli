use std::collections::HashMap;

use chrono_humanize::HumanTime;

use crate::{
    commands::{
        output::service_summary::{
            build_service_output, fetch_region_locations, format_size_pair, print_service_card,
            service_resource_details,
        },
        queries::project::{
            ProjectProject, ProjectProjectEnvironmentsEdges,
            ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
        },
    },
    controllers::{
        config::{EnvironmentConfig, environment::fetch_environment_config},
        project::{ensure_project_and_environment_exist, get_project},
    },
    resources::{
        ResourceKind, classify_service_instance, database_label, name_mentions, project_bucket_name,
    },
};

use super::*;

/// Show information about the current project
#[derive(Parser)]
pub struct Args {
    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.to_owned()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&project)?);
        return Ok(());
    }

    let region_locations = fetch_region_locations(&client, &configs).await;

    let environment = linked_project.environment.as_deref().and_then(|eid| {
        project
            .environments
            .edges
            .iter()
            .find(|env| env.node.id == eid)
    });

    let environment_config = if let Some(environment) = environment {
        Some(
            fetch_environment_config(&client, &configs, &environment.node.id, false)
                .await?
                .config,
        )
    } else {
        None
    };

    print_context(&project, environment);
    print_linked_service(&project, &linked_project, environment, &region_locations);
    if let (Some(environment), Some(environment_config)) =
        (environment, environment_config.as_ref())
    {
        print_divider();
        print_project_resources(&project, environment, environment_config, &region_locations);
    }
    println!();

    Ok(())
}

const FIELD_LABEL_WIDTH: usize = 16;

fn print_field(label: &str, value: &impl std::fmt::Display) {
    let padded = format!("{label:<FIELD_LABEL_WIDTH$}");
    println!("{} {value}", padded.dimmed());
}

fn print_indented_field(label: &str, value: &impl std::fmt::Display) {
    let padded = format!("{label:<FIELD_LABEL_WIDTH$}");
    println!("    {} {value}", padded.dimmed());
}

fn print_divider() {
    println!("{}", "─".repeat(48).dimmed());
    println!();
}

fn print_context(project: &ProjectProject, environment: Option<&ProjectProjectEnvironmentsEdges>) {
    println!();
    if let Some(workspace) = &project.workspace {
        print_field("Workspace:", &workspace.name);
        println!();
    }

    print_field("Project:", &project.name.purple().bold());
    print_field("Project ID:", &project.id.clone().dimmed());
    println!();

    if let Some(environment) = environment {
        print_field("Environment:", &environment.node.name.blue().bold());
        print_field("Environment ID:", &environment.node.id.clone().dimmed());
        if let Some(count) = environment.node.unmerged_changes_count.filter(|&c| c > 0) {
            let label = if count == 1 { "change" } else { "changes" };
            print_field("Unmerged:", &format!("{count} {label}").yellow());
        }
    } else {
        print_field("Environment:", &"None".red().bold());
    }
}

fn print_linked_service(
    project: &ProjectProject,
    linked_project: &LinkedProject,
    environment: Option<&ProjectProjectEnvironmentsEdges>,
    region_locations: &HashMap<String, String>,
) {
    println!();
    println!("{}", "Linked service".bold());
    println!();

    let Some(linked_service_id) = linked_project.service.as_deref() else {
        print_indented_field("Service:", &"None".red().bold());
        println!();
        return;
    };

    let Some(service) = project
        .services
        .edges
        .iter()
        .find(|service| service.node.id == linked_service_id)
    else {
        print_indented_field(
            "Service:",
            &format!("{linked_service_id} (not found in project, run `railway service` to relink)")
                .yellow()
                .bold(),
        );
        println!();
        return;
    };

    let Some(environment) = environment else {
        print_indented_field("Service:", &service.node.name.green().bold());
        print_indented_field("Service ID:", &service.node.id.clone().dimmed());
        println!();
        return;
    };

    let in_environment = environment
        .node
        .service_instances
        .edges
        .iter()
        .any(|instance| instance.node.service_id == linked_service_id);
    if !in_environment {
        print_indented_field("Service:", &service.node.name.green().bold());
        print_indented_field("Service ID:", &service.node.id.clone().dimmed());
        print_indented_field(
            "Status:",
            &"not found in linked environment, run `railway service` to relink".yellow(),
        );
        println!();
        return;
    }

    let row = build_service_output(
        project,
        &environment.node.id,
        &service.node,
        Some(linked_service_id),
        region_locations,
    );
    print_service_card(&row, false);
}

fn print_project_resources(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    environment_config: &EnvironmentConfig,
    region_locations: &HashMap<String, String>,
) {
    println!("{}", "All resources".bold());
    println!();
    print_resource_section(
        "Services",
        service_resources(project, environment, region_locations),
    );
    print_resource_section(
        "Databases",
        database_resources(project, environment, region_locations),
    );
    print_resource_section("Volumes", detached_volume_resources(environment));
    print_resource_section(
        "Functions",
        function_resources(project, environment, region_locations),
    );
    print_resource_section(
        "Cron jobs",
        cron_job_resources(project, environment, region_locations),
    );
    print_resource_section("Buckets", bucket_resources(project, environment_config));
}

struct ResourceLine {
    name: String,
    details: Vec<String>,
}

fn print_resource_section(label: &str, resources: Vec<ResourceLine>) {
    if resources.is_empty() {
        return;
    }

    println!("    {}", label.bold());
    for resource in resources {
        if resource.details.is_empty() {
            println!("      - {}", resource.name);
        } else {
            println!(
                "      - {}: {}",
                resource.name,
                resource.details.join(&format!(" {} ", "·".dimmed()))
            );
        }
    }
}

fn service_resources(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    region_locations: &HashMap<String, String>,
) -> Vec<ResourceLine> {
    environment
        .node
        .service_instances
        .edges
        .iter()
        .filter(|service| classify_service_instance(service) == ResourceKind::Service)
        .map(|service| resource_line(project, environment, service, region_locations))
        .collect()
}

fn database_resources(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    region_locations: &HashMap<String, String>,
) -> Vec<ResourceLine> {
    environment
        .node
        .service_instances
        .edges
        .iter()
        .filter(|service| classify_service_instance(service) == ResourceKind::Database)
        .map(|service| {
            let name = &service.node.service_name;
            let name = if let Some(label) =
                database_label(service).filter(|label| !name_mentions(name, label))
            {
                format!("{name} ({label})")
            } else {
                name.clone()
            };
            resource_line_with_name(project, environment, service, name, region_locations)
        })
        .collect()
}

fn detached_volume_resources(environment: &ProjectProjectEnvironmentsEdges) -> Vec<ResourceLine> {
    environment
        .node
        .volume_instances
        .edges
        .iter()
        .filter(|instance| instance.node.service_id.is_none())
        .map(|instance| {
            let mut details = vec!["detached".yellow().to_string()];
            details.push(format_size_pair(
                instance.node.current_size_mb,
                instance.node.size_mb as f64,
            ));
            if let Some(state) = &instance.node.state {
                details.push(format!("{state:?}").to_lowercase());
            }

            ResourceLine {
                name: instance.node.volume.name.clone(),
                details,
            }
        })
        .collect()
}

fn function_resources(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    region_locations: &HashMap<String, String>,
) -> Vec<ResourceLine> {
    environment
        .node
        .service_instances
        .edges
        .iter()
        .filter(|service| classify_service_instance(service) == ResourceKind::Function)
        .map(|function| ResourceLine {
            name: function.node.service_name.clone(),
            details: resource_details(project, environment, function, region_locations),
        })
        .collect()
}

fn cron_job_resources(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    region_locations: &HashMap<String, String>,
) -> Vec<ResourceLine> {
    environment
        .node
        .service_instances
        .edges
        .iter()
        .filter(|service| classify_service_instance(service) == ResourceKind::CronJob)
        .map(|service| {
            let mut details = resource_details(project, environment, service, region_locations);
            if let Some(schedule) = &service.node.cron_schedule {
                details.push(schedule.clone());
            }
            if let Some(next_run) = service.node.next_cron_run_at {
                details.push(format!("next run {}", HumanTime::from(next_run)));
            }
            ResourceLine {
                name: service.node.service_name.clone(),
                details,
            }
        })
        .collect()
}

fn bucket_resources(
    project: &ProjectProject,
    environment_config: &EnvironmentConfig,
) -> Vec<ResourceLine> {
    let mut resources: Vec<_> = environment_config
        .buckets
        .iter()
        .filter(|(_, config)| config.is_deleted != Some(true))
        .map(|(bucket_id, _)| ResourceLine {
            name: project_bucket_name(project, bucket_id).unwrap_or_else(|| bucket_id.clone()),
            details: Vec::new(),
        })
        .collect();

    resources.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    resources
}

fn resource_line(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    service: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
    region_locations: &HashMap<String, String>,
) -> ResourceLine {
    resource_line_with_name(
        project,
        environment,
        service,
        service.node.service_name.clone(),
        region_locations,
    )
}

fn resource_line_with_name(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    service: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
    name: String,
    region_locations: &HashMap<String, String>,
) -> ResourceLine {
    ResourceLine {
        name,
        details: resource_details(project, environment, service, region_locations),
    }
}

fn resource_details(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    service: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
    region_locations: &HashMap<String, String>,
) -> Vec<String> {
    let Some(service_edge) = project
        .services
        .edges
        .iter()
        .find(|edge| edge.node.id == service.node.service_id)
    else {
        return vec!["service metadata unavailable".yellow().to_string()];
    };

    let row = build_service_output(
        project,
        &environment.node.id,
        &service_edge.node,
        None,
        region_locations,
    );
    service_resource_details(&row)
}
