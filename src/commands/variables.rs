use super::*;
use crate::{
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
    table::Table,
};

/// Show variables for active environment
#[derive(Parser)]
pub struct Args {
    /// Service to show variables for
    #[clap(short, long)]
    service: Option<String>,

    /// Show variables in KV format
    #[clap(short, long)]
    kv: bool,
}

pub async fn command(args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let _vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let (vars, name) = if let Some(ref service) = args.service {
        let service_name = project
            .services
            .edges
            .iter()
            .find(|edge| edge.node.id == *service || edge.node.name == *service)
            .ok_or_else(|| RailwayError::ServiceNotFound(service.clone()))?;
        (
            queries::variables_for_service_deployment::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: service_name.node.id.clone(),
            },
            service_name.node.name.clone(),
        )
    } else if let Some(ref service) = linked_project.service {
        let service_name = project
            .services
            .edges
            .iter()
            .find(|edge| edge.node.id == *service)
            .ok_or_else(|| RailwayError::ServiceNotFound(service.clone()))?;
        (
            queries::variables_for_service_deployment::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: service.clone(),
            },
            service_name.node.name.clone(),
        )
    } else {
        return Err(RailwayError::NoServiceLinked.into());
    };

    let variables = post_graphql::<queries::VariablesForServiceDeployment, _>(
        &client,
        configs.get_backboard(),
        vars,
    )
    .await?
    .variables_for_service_deployment;

    if variables.is_empty() {
        eprintln!("No variables found");
        return Ok(());
    }

    if args.kv {
        for (key, value) in variables {
            println!("{}={}", key, value);
        }
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&variables)?);
        return Ok(());
    }

    let table = Table::new(name, variables);
    table.print()?;

    Ok(())
}
