use super::*;
use crate::{
    mutations::service_create::ServiceSourceInput,
    subscription::subscribe_graphql,
    subscriptions::deployment::DeploymentStatus,
    util::{
        progress::{create_spinner, success_spinner},
        prompt::{
            fake_select, fake_select_cancelled, prompt_confirm_with_default, prompt_path,
            prompt_text, prompt_text_skippable,
        },
    },
};
use anyhow::bail;
use base64::prelude::*;
use futures::StreamExt;
use indoc::{formatdoc, writedoc};
use is_terminal::IsTerminal;
use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult};
use queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges};
use std::io::Write as _;
use std::path::Path;
use std::{fmt::Write as _, sync::Arc};
use tokio_util::sync::CancellationToken;

pub async fn new(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
    args: New,
) -> Result<()> {
    let args = prompt(args)?;
    let (service_id, domain) = create_function_service(&args, environment, &project).await?;
    let info = format_function_info(&args, &project, environment, &domain);

    if args.watch {
        watch_for_file_changes(args, service_id, environment, info).await?;
    } else {
        println!("{}", info);
    }

    Ok(())
}

fn get_start_cmd(path: &Path) -> Result<String> {
    let content = std::fs::read(path)?;
    let cmd = format!("./run.sh {}", BASE64_STANDARD.encode(content));

    if cmd.len() >= 96 * 1024 {
        bail!("Your function is too large (must be smaller than 96kb base64)");
    }

    Ok(cmd)
}

async fn create_function_service(
    args: &Arguments,
    environment: &ProjectProjectEnvironmentsEdges,
    project: &ProjectProject,
) -> Result<(String, Option<String>)> {
    let cmd = get_start_cmd(&args.path)?;
    let mut spinner = create_spinner("Creating function".into());

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let latest_version = get_latest_function_version(&client, &configs).await?;
    let service_id = create_service(
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
    link_function(&args.path, &service_id)?;

    success_spinner(&mut spinner, "Function created".into());
    Ok((service_id, domain))
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
) -> Result<String> {
    let service = post_graphql::<mutations::ServiceCreate, _>(
        client,
        configs.get_backboard(),
        mutations::service_create::Variables {
            branch: None,
            name: Some(args.name.clone()),
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
    .service_create
    .id;

    Ok(service)
}

fn link_function(path: &Path, id: &str) -> Result<()> {
    let mut c = Configs::new()?;
    c.link_function(path.to_path_buf(), id.to_owned())?;
    c.write()?;
    Ok(())
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

fn format_function_info(
    args: &Arguments,
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    domain: &Option<String>,
) -> String {
    let mut info = formatdoc!(
        "
        Name: {}
        Project: {}
        Environment: {}
        Local file: {}
        ",
        args.name.blue(),
        project.name.blue(),
        environment.node.name.clone().blue(),
        args.path.display().to_string().blue()
    );

    append_domain_info(&mut info, domain);
    append_cron_info(&mut info, &args.cron);

    info
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

async fn watch_for_file_changes(
    args: Arguments,
    service_id: String,
    environment: &ProjectProjectEnvironmentsEdges,
    info: String,
) -> Result<()> {
    let (watch_tx, file_change) = tokio::sync::mpsc::unbounded_channel();
    let debounce_tx = watch_tx.clone();

    let _debouncer = setup_file_watcher(&args.path, debounce_tx)?;
    watch_tx.send(Ok(()))?;

    let service_arc = Arc::new(service_id);
    let env_arc = Arc::new(environment.node.id.clone());

    display_initial_info(&args, &info)?;

    run_watch_loop(args, service_arc, env_arc, info, file_change).await
}

fn setup_file_watcher(
    path: &Path,
    tx: tokio::sync::mpsc::UnboundedSender<Result<(), notify::Error>>,
) -> Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
    let mut debouncer = new_debouncer(
        std::time::Duration::from_secs(5),
        move |res: DebounceEventResult| {
            handle_debounced_events(res, &tx);
        },
    )?;

    debouncer
        .watcher()
        .watch(path, RecursiveMode::NonRecursive)?;
    Ok(debouncer)
}

fn display_initial_info(args: &Arguments, info: &str) -> Result<()> {
    if args.terminal {
        clear()?;
        println!("{}", info);
    }
    Ok(())
}

async fn run_watch_loop(
    args: Arguments,
    service_id: Arc<String>,
    environment_id: Arc<String>,
    info: String,
    mut file_change: tokio::sync::mpsc::UnboundedReceiver<Result<(), notify::Error>>,
) -> Result<()> {
    let mut current_cancel_token: Option<CancellationToken> = None;
    let mut first_run = true;

    loop {
        tokio::select! {
            event = file_change.recv() => {
                if first_run {
                    first_run = false;
                    continue;
                }

                if let Some(Ok(_)) = event {
                    current_cancel_token = handle_file_change_event(
                        &args,
                        &service_id,
                        &environment_id,
                        &info,
                        current_cancel_token,
                    ).await?;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nStopping file watcher...");
                if let Some(token) = current_cancel_token.take() {
                    token.cancel();
                }
                break;
            }
        }
    }

    Ok(())
}

fn handle_debounced_events(
    res: DebounceEventResult,
    tx: &tokio::sync::mpsc::UnboundedSender<Result<(), notify::Error>>,
) {
    match res {
        Ok(events) if !events.is_empty() => {
            if let Err(e) = tx.send(Ok(())) {
                eprintln!("Failed to send debounced file event: {}", e);
            }
        }
        Err(e) => {
            if let Err(send_err) = tx.send(Err(e)) {
                eprintln!("Failed to send debounced error: {}", send_err);
            }
        }
        _ => {} // Empty events, ignore
    }
}

async fn handle_file_change_event(
    args: &Arguments,
    service_id: &Arc<String>,
    environment_id: &Arc<String>,
    info: &str,
    current_cancel_token: Option<CancellationToken>,
) -> Result<Option<CancellationToken>> {
    // Cancel any existing deployment stream
    if let Some(token) = current_cancel_token {
        token.cancel();
    }

    let mut spinner = create_spinner("Updating function".into());
    let cmd = match get_start_cmd(&args.path) {
        Ok(cmd) => cmd,
        Err(_) => {
            println!("{}: Your function is too large", "ERROR".red());
            return Ok(None);
        }
    };

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    // Update function
    post_graphql::<mutations::FunctionUpdate, _>(
        &client,
        configs.get_backboard(),
        mutations::function_update::Variables {
            service_id: service_id.as_ref().clone(),
            environment_id: environment_id.as_ref().clone(),
            sleep_application: None,
            cron_schedule: None,
            start_command: Some(cmd),
        },
    )
    .await?;

    // Deploy function
    let latest = post_graphql::<mutations::ServiceInstanceDeploy, _>(
        &client,
        configs.get_backboard(),
        mutations::service_instance_deploy::Variables {
            environment_id: environment_id.as_ref().clone(),
            service_id: service_id.as_ref().clone(),
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

    // Create new cancellation token and spawn deployment monitoring task
    let cancel_token = CancellationToken::new();
    let info_clone = info.to_string();
    let terminal = args.terminal;

    tokio::spawn(monitor_deployment_status(
        stream,
        cancel_token.clone(),
        info_clone,
        terminal,
    ));

    Ok(Some(cancel_token))
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
    terminal: bool,
) {
    tokio::pin!(stream);

    loop {
        tokio::select! {
            stream_item = stream.next() => {
                match stream_item {
                    Some(Ok(stream_data)) => {
                        if let Some(data) = stream_data.data {
                            let deployment = data.deployment;
                            // dbg!(&deployment.status);
                            // if matches!(deployment.status, DeploymentStatus::CRASHED) {
                            //     show_error_information(&info, &deployment).await.unwrap();
                            // }
                            if terminal {
                                display_deployment_info(&info, &deployment.status);
                            }


                        }
                    }
                    Some(Err(_)) | None => break,
                }
            }
            _ = cancel_token.cancelled() => break,
        }
    }
}

// async fn show_error_information(base_info: &str, deployment: &DeploymentDeployment) -> Result<()> {
//     let configs = Configs::new()?;
//     let client = GQLClient::new_authorized(&configs)?;
//     let deploy_logs = post_graphql::<queries::DeploymentLogs, _>(&client, configs.get_backboard(), queries::deployment_logs::Variables {
//         deployment_id: deployment.id.clone()
//     }).await?;
//     dbg!(deploy_logs);
//     Ok(())
// }

fn display_deployment_info(base_info: &str, status: &DeploymentStatus) {
    if clear().is_err() {
        return;
    }

    let mut info = base_info.to_string();
    let status_display = format_deployment_status(status);
    let timestamp = format_current_timestamp();

    if writedoc!(
        &mut info,
        "Latest deployment: {} ({})",
        status_display,
        timestamp
    )
    .is_ok()
    {
        println!("{}", info);
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

fn clear() -> Result<(), anyhow::Error> {
    print!("\x1B[2J\x1B[1;1H");
    std::io::stdout().flush()?;
    Ok(())
}

struct Arguments {
    terminal: bool,
    name: String,
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
        name
    } else if terminal {
        prompt_text("Enter a name for your function")?
    } else {
        bail!("Name must be provided when not running in a terminal");
    };
    let path = if let Some(path) = args.path {
        fake_select(
            "Enter the path to your function",
            &path.display().to_string(),
        );
        path
    } else if terminal {
        prompt_path("Enter the path of your function")?
    } else {
        bail!("Path must be provided when not running in a terminal");
    };
    if !path.exists() {
        println!("Provided path doesn't exist, creating file");
        std::fs::write(&path, "")?;
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
        fake_select_cancelled("Enter a cron schedule");
        None
    } else if let Some(cron) = args.cron {
        fake_select("Enter a cron schedule", &cron);
        Some(cron)
    } else if terminal {
        prompt_text_skippable("Enter a cron schedule <esc to skip>")?
    } else {
        None
    }
    .map(|s| s.trim().to_string());
    if let Some(cron) = &cron {
        let schedule = croner::Cron::new(cron).parse();
        if let Ok(schedule) = schedule {
            let now = chrono::Utc::now();

            // Get the next 2 runs
            let first = schedule.find_next_occurrence(&now, false)?;
            let second = schedule.find_next_occurrence(&first, false)?;
            let interval = second.signed_duration_since(first);
            if interval < chrono::Duration::minutes(5) {
                bail!("Cron schedule runs too frequently (every {} minutes). Minimum interval is 5 minutes.", interval.num_minutes())
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
