use super::*;
use crate::{
    mutations::service_create::ServiceSourceInput,
    subscription::subscribe_graphql,
    subscriptions::deployment::DeploymentStatus,
    util::{
        progress::{create_spinner, success_spinner},
        prompt::{
            fake_select, prompt_confirm_with_default, prompt_path_with_default,
            prompt_text_with_placeholder_if_blank,
        },
        watcher::FileWatcher,
    },
};
use anyhow::bail;
use futures::StreamExt;
use indoc::{formatdoc, writedoc};
use is_terminal::IsTerminal;
use queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges};
use std::io::Write as _;
use std::{fmt::Write as _, path::Path};
use tokio_util::sync::CancellationToken;
pub async fn new(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
    args: New,
) -> Result<()> {
    let args = prompt(args)?;
    let ((service_id, service_name), domain) =
        create_function_service(&args, environment, &project).await?;
    let info = format_function_info(
        &project,
        environment,
        &domain,
        service_name,
        service_id.clone(),
        &args.path,
        args.cron,
    )?;

    if args.watch {
        watch_for_file_changes(
            project,
            service_id,
            environment,
            info,
            args.path,
            args.terminal,
        )
        .await?;
    } else {
        println!("{info}");
    }

    Ok(())
}

async fn create_function_service(
    args: &Arguments,
    environment: &ProjectProjectEnvironmentsEdges,
    project: &ProjectProject,
) -> Result<((String, String), Option<String>)> {
    let cmd = common::get_start_cmd(&args.path)?;
    let mut spinner = create_spinner("Creating function".into());

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let latest_version = get_latest_function_version(&client, &configs).await?;
    let (service_id, service_name) = create_service(
        &client,
        &configs,
        args,
        environment,
        project,
        &latest_version,
    )
    .await?;
    update_function_settings(&client, &configs, &service_id, args, environment, &cmd).await?;
    let domain = create_domain_if_requested(
        &client,
        &configs,
        &service_id,
        environment,
        project,
        args.domain,
    )
    .await?;
    common::link_function(&args.path, &service_id)?;

    success_spinner(&mut spinner, "Function created".into());
    Ok(((service_id, service_name), domain))
}

async fn get_latest_function_version(
    client: &reqwest::Client,
    configs: &Configs,
) -> Result<String> {
    let latest = post_graphql::<queries::LatestFunctionVersion, _>(
        client,
        configs.get_backboard(),
        queries::latest_function_version::Variables {},
    )
    .await?
    .function_runtime
    .latest_version;

    Ok(latest.image)
}

async fn create_service(
    client: &reqwest::Client,
    configs: &Configs,
    args: &Arguments,
    environment: &ProjectProjectEnvironmentsEdges,
    project: &ProjectProject,
    image: &str,
) -> Result<(String, String)> {
    let service = post_graphql::<mutations::ServiceCreate, _>(
        client,
        configs.get_backboard(),
        mutations::service_create::Variables {
            branch: None,
            name: args.name.clone(),
            project_id: project.id.clone(),
            environment_id: environment.node.id.clone(),
            source: Some(ServiceSourceInput {
                image: Some(image.to_string()),
                repo: None,
            }),
            variables: None,
        },
    )
    .await?
    .service_create;

    Ok((service.id, service.name))
}

async fn update_function_settings(
    client: &reqwest::Client,
    configs: &Configs,
    service_id: &str,
    args: &Arguments,
    environment: &ProjectProjectEnvironmentsEdges,
    cmd: &str,
) -> Result<()> {
    post_graphql::<mutations::FunctionUpdate, _>(
        client,
        configs.get_backboard(),
        mutations::function_update::Variables {
            service_id: service_id.to_string(),
            environment_id: environment.node.id.clone(),
            sleep_application: Some(args.serverless),
            cron_schedule: args.cron.clone(),
            start_command: Some(cmd.to_string()),
        },
    )
    .await?;

    Ok(())
}

async fn create_domain_if_requested(
    client: &reqwest::Client,
    configs: &Configs,
    service_id: &str,
    environment: &ProjectProjectEnvironmentsEdges,
    project: &ProjectProject,
    should_create: bool,
) -> Result<Option<String>> {
    if !should_create {
        return Ok(None);
    }

    post_graphql::<mutations::ServiceDomainCreate, _>(
        client,
        configs.get_backboard(),
        mutations::service_domain_create::Variables {
            service_id: service_id.to_string(),
            environment_id: environment.node.id.clone(),
        },
    )
    .await?;

    let domains_response = post_graphql::<queries::Domains, _>(
        client,
        configs.get_backboard(),
        queries::domains::Variables {
            environment_id: environment.node.id.clone(),
            service_id: service_id.to_string(),
            project_id: project.id.clone(),
        },
    )
    .await?;

    let domain = domains_response
        .domains
        .service_domains
        .first()
        .map(|d| d.domain.clone());

    Ok(domain)
}

pub fn format_function_info(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    domain: &Option<String>,
    name: String,
    service_id: String,
    path: &Path,
    cron: Option<String>,
) -> Result<String> {
    let path = pathdiff::diff_paths(path.canonicalize()?, std::env::current_dir()?)
        .map(|f| f.display().to_string())
        .unwrap_or(path.display().to_string());
    let mut info = formatdoc!(
        "
        Name: {}
        Project: {}
        Environment: {}
        Local file: {}
        Link: {}
        ",
        name.blue(),
        project.name.blue(),
        environment.node.name.clone().blue(),
        path.blue(),
        format!(
            "https://railway.com/project/{}/service/{}?environmentId={}",
            project.id, service_id, environment.node.id
        )
        .blue()
    );

    append_domain_info(&mut info, domain);
    append_cron_info(&mut info, &cron);

    Ok(info)
}

fn append_domain_info(info: &mut String, domain: &Option<String>) {
    if let Some(domain) = domain {
        writedoc!(
            info,
            "
            Domain: {}{}
            ",
            "https://".blue(),
            domain.blue()
        )
        .expect("Failed to write domain info");
    }
}

fn append_cron_info(info: &mut String, cron: &Option<String>) {
    if let Some(cron) = cron {
        let description =
            cron_descriptor::cronparser::cron_expression_descriptor::get_description_cron(cron)
                .expect("cron is not valid");
        writedoc!(
            info,
            "
            Cron: {} ({})
            ",
            description.blue(),
            cron.blue()
        )
        .expect("Failed to write cron info");
    }
}

pub async fn watch_for_file_changes(
    project: ProjectProject,
    service_id: String,
    environment: &ProjectProjectEnvironmentsEdges,
    info: String,
    path: PathBuf,
    terminal: bool,
) -> Result<()> {
    if terminal {
        clear()?;
        if let Some(f) = common::find_service(&project, environment, &service_id) {
            if let Some(ld) = f.latest_deployment {
                display_deployment_info(info.as_str(), &ld.status.into(), ld.deployment_stopped);
            }
        } else {
            println!("{info}");
        }
    }
    let watcher = FileWatcher::new(path.clone());
    let environment_id = environment.node.id.clone();
    watcher
        .watch(move |token, event| {
            let service_id = service_id.clone();
            let environment_id = environment_id.clone();
            let path = path.clone();
            // Only clone info if we're in terminal mode
            let info = if terminal { Some(info.clone()) } else { None };
            async move {
                if !matches!(
                    event.kind,
                    notify::EventKind::Modify(notify::event::ModifyKind::Data(_))
                ) {
                    return Ok(());
                }

                match handle_function_change(service_id, environment_id, path, info, token).await {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        eprintln!("Error handling function change: {e}");
                        Ok(())
                    }
                }
            }
        })
        .await
}

async fn handle_function_change(
    service_id: String,
    environment_id: String,
    path: std::path::PathBuf,
    info: Option<String>,
    token: CancellationToken,
) -> Result<()> {
    let mut spinner = create_spinner("Updating function".into());
    let cmd = match common::get_start_cmd(&path) {
        Ok(cmd) => cmd,
        Err(_) => {
            println!("{}: Your function is too large", "ERROR".red());
            return Err(anyhow::anyhow!("Function too big (max size of 96kb)"));
        }
    };

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    post_graphql_skip_none::<mutations::FunctionUpdate, _>(
        &client,
        configs.get_backboard(),
        mutations::function_update::Variables {
            service_id: service_id.clone(),
            environment_id: environment_id.clone(),
            sleep_application: None,
            cron_schedule: None,
            start_command: Some(cmd),
        },
    )
    .await?;

    let latest = post_graphql::<mutations::ServiceInstanceDeploy, _>(
        &client,
        configs.get_backboard(),
        mutations::service_instance_deploy::Variables {
            environment_id,
            service_id,
        },
    )
    .await?
    .service_instance_deploy_v2;

    let stream =
        subscribe_graphql::<subscriptions::Deployment>(subscriptions::deployment::Variables {
            id: latest,
        })
        .await?;

    success_spinner(&mut spinner, "Function updated".into());

    if let Some(info) = info {
        tokio::spawn(monitor_deployment_status(stream, token, info));
    }

    Ok(())
}

fn clear() -> Result<(), anyhow::Error> {
    print!("\x1B[2J\x1B[1;1H");
    std::io::stdout().flush()?;
    Ok(())
}

async fn monitor_deployment_status(
    stream: impl futures::Stream<
        Item = Result<
            graphql_client::Response<subscriptions::deployment::ResponseData>,
            graphql_ws_client::Error,
        >,
    > + Unpin,
    cancel_token: CancellationToken,
    info: String,
) {
    tokio::pin!(stream);

    loop {
        tokio::select! {
            stream_item = stream.next() => {
                match stream_item {
                    Some(Ok(stream_data)) => {
                        if let Some(data) = stream_data.data {
                            let deployment = data.deployment;
                            display_deployment_info(&info, &deployment.status, deployment.deployment_stopped);
                        }
                    }
                    Some(Err(_)) | None => break,
                }
            }
            _ = cancel_token.cancelled() => break,
        }
    }
}

fn display_deployment_info(base_info: &str, status: &DeploymentStatus, stopped: bool) {
    if clear().is_err() {
        return;
    }

    let status_display = if stopped && matches!(status, DeploymentStatus::SUCCESS) {
        "COMPLETED".green()
    } else {
        format_deployment_status(status)
    };

    let timestamp = format_current_timestamp();

    let mut info = String::with_capacity(base_info.len() + 50);
    info.push_str(base_info);

    if writedoc!(
        &mut info,
        "Latest deployment: {} ({})",
        status_display,
        timestamp
    )
    .is_ok()
    {
        println!("{info}");
    }
}

fn format_deployment_status(status: &DeploymentStatus) -> colored::ColoredString {
    let status_str = serde_json::to_string(status)
        .unwrap_or_else(|_| "UNKNOWN".to_string())
        .replace('"', "");

    match status {
        DeploymentStatus::BUILDING
        | DeploymentStatus::DEPLOYING
        | DeploymentStatus::INITIALIZING
        | DeploymentStatus::QUEUED => status_str.blue(),
        DeploymentStatus::CRASHED | DeploymentStatus::FAILED => status_str.red(),
        DeploymentStatus::SLEEPING => status_str.yellow(),
        DeploymentStatus::SUCCESS => status_str.green(),
        _ => status_str.dimmed(),
    }
}

fn format_current_timestamp() -> colored::ColoredString {
    chrono::Local::now()
        .format("%I:%M:%S %p")
        .to_string()
        .dimmed()
}

struct Arguments {
    terminal: bool,
    name: Option<String>,
    path: PathBuf,
    domain: bool,
    cron: Option<String>,
    serverless: bool,
    watch: bool,
}

fn prompt(args: New) -> Result<Arguments> {
    let terminal = std::io::stdout().is_terminal();
    let name = if let Some(name) = args.name {
        fake_select("Enter a name for your function", &name);
        Some(name)
    } else if terminal {
        Some(prompt_text_with_placeholder_if_blank(
            "Enter a name for your function",
            "<leave blank for randomly generated>",
            "<randomly generated>",
        )?)
        .filter(|s| !s.trim().is_empty())
    } else {
        fake_select("Enter a function name", "<randomly generated>");
        None
    };
    let path = if let Some(path) = args.path {
        fake_select("Enter the path to your function", &path.to_string_lossy());
        path
    } else if terminal {
        prompt_path_with_default("Enter the path of your function", "./bun-function.ts")?
    } else {
        bail!("Path must be provided when not running in a terminal");
    };
    if !path.exists() {
        println!("Provided path doesn't exist, creating file");
        std::fs::write(&path, "console.log(`Hello from Bun v${Bun.version}!`)")?;
    }
    let domain = if args.cron.is_some() {
        fake_select("Generate a domain?", "No");
        false
    } else if let Some(http) = args.http {
        fake_select("Generate a domain?", if http { "Yes" } else { "No" });
        http
    } else if terminal {
        prompt_confirm_with_default("Generate a domain?", true)?
    } else {
        false
    };
    let cron = if domain {
        fake_select(
            "Enter a cron schedule",
            "<domain chosen; skipped cron option>",
        );
        None
    } else if let Some(cron) = args.cron {
        fake_select("Enter a cron schedule", &cron);
        Some(cron)
    } else if terminal {
        Some(prompt_text_with_placeholder_if_blank(
            "Enter a cron schedule",
            "<leave blank to skip>",
            "<no cron>",
        )?)
        .filter(|s| !s.trim().is_empty())
    } else {
        None
    }
    .map(|s| s.trim().to_owned());
    if let Some(cron) = &cron {
        let schedule = croner::Cron::new(cron).parse();
        if let Ok(schedule) = schedule {
            let now = chrono::Utc::now();

            // Get the next 2 runs
            let first = schedule.find_next_occurrence(&now, false)?;
            let second = schedule.find_next_occurrence(&first, false)?;
            let interval = second.signed_duration_since(first);
            if interval < chrono::Duration::minutes(5) {
                bail!(
                    "Cron schedule runs too frequently (every {} minutes). Minimum interval is 5 minutes.",
                    interval.num_minutes()
                )
            }
        } else {
            bail!("Invalid cron schedule")
        }
    }
    let serverless = if let Some(serverless) = args.serverless {
        fake_select(
            "Should this function be serverless?",
            if serverless { "Yes" } else { "No" },
        );
        serverless
    } else if terminal {
        prompt_confirm_with_default("Should this function be serverless?", true)?
    } else {
        false
    };
    let watch = if let Some(watch) = args.watch {
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
    };
    Ok(Arguments {
        terminal,
        name,
        path,
        domain,
        cron,
        serverless,
        watch,
    })
}
