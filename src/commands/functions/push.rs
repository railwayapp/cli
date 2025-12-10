use crate::{
    queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges},
    util::prompt::{fake_select, prompt_confirm_with_default},
};
use is_terminal::IsTerminal;

use super::*;

pub async fn push(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
    args: Push,
) -> Result<()> {
    let (id, path) = common::get_function_from_path(args.path.clone())?;
    let service = common::find_service(environment, &id)
        .ok_or_else(|| anyhow::anyhow!("Couldn't find service"))?;
    let terminal = std::io::stdout().is_terminal();

    if should_watch(args, terminal)? {
        let info = new::format_function_info(
            &project,
            environment,
            &domain(&service),
            service.service_name,
            service.service_id,
            path.as_path(),
            service.cron_schedule,
        )?;
        new::watch_for_file_changes(id.clone(), environment, info, path, terminal).await?
    } else {
        println!("Updating function {}", service.service_name.blue().bold());

        let configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        let new_cmd = common::get_start_cmd(&path)?;

        let diff_stats = common::calculate_diff(
            &common::extract_function_content(&service)?,
            &String::from_utf8(std::fs::read(&path)?)?,
        );

        update_function(&client, &configs, &environment.node.id, &id, new_cmd).await?;
        deploy_function(&client, &configs, &environment.node.id, &id).await?;

        println!("Function updated {diff_stats}");
    }
    Ok(())
}

async fn update_function(
    client: &reqwest::Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    start_command: String,
) -> Result<()> {
    post_graphql_skip_none::<mutations::FunctionUpdate, _>(
        client,
        configs.get_backboard(),
        mutations::function_update::Variables {
            environment_id: environment_id.to_string(),
            service_id: service_id.to_string(),
            start_command: Some(start_command),
            cron_schedule: None,
            sleep_application: None,
        },
    )
    .await?;
    Ok(())
}

async fn deploy_function(
    client: &reqwest::Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<()> {
    post_graphql::<mutations::ServiceInstanceDeploy, _>(
        client,
        configs.get_backboard(),
        mutations::service_instance_deploy::Variables {
            environment_id: environment_id.to_string(),
            service_id: service_id.to_string(),
        },
    )
    .await?;
    Ok(())
}

fn should_watch(args: Push, terminal: bool) -> Result<bool> {
    Ok(if let Some(watch) = args.watch {
        fake_select(
            "Do you want to watch for changes and automatically redeploy?",
            if watch { "Yes" } else { "No" },
        );
        watch
    } else if terminal {
        prompt_confirm_with_default(
            "Do you want to watch for changes and automatically redeploy?",
            true,
        )?
    } else {
        false
    })
}

fn domain(
    service: &queries::project::ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdgesNode,
) -> Option<String> {
    let mut domains = service
        .domains
        .custom_domains
        .iter()
        .map(|f| f.domain.clone())
        .chain(
            service
                .domains
                .service_domains
                .iter()
                .map(|f| f.domain.clone()),
        );
    domains.next()
}
