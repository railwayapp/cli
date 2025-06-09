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
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges};
use std::io::Write as _;
use std::{fmt::Write as _, sync::Arc};
use tokio_util::sync::CancellationToken;

pub async fn new(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
    args: New,
) -> Result<()> {
    let Arguments {
        terminal,
        name,
        path,
        domain,
        cron,
        serverless,
        watch,
    } = prompt(args)?;

    let cmd = get_start_cmd(&path)?;
    if cmd.len() >= 96 * 1024 {
        bail!("Your function is too large (must be smaller than 96kb base64");
    }
    let mut spinner = create_spinner("Creating function".into());
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let latest = post_graphql::<queries::LatestFunctionVersion, _>(
        &client,
        configs.get_backboard(),
        queries::latest_function_version::Variables {},
    )
    .await?
    .function_runtime
    .latest_version;
    let service = post_graphql::<mutations::ServiceCreate, _>(
        &client,
        configs.get_backboard(),
        mutations::service_create::Variables {
            branch: None,
            name: Some(name.clone()),
            project_id: project.id.clone(),
            environment_id: environment.node.id.clone(),
            source: Some(ServiceSourceInput {
                image: Some(latest.image),
                repo: None,
            }),
            variables: None,
        },
    )
    .await?
    .service_create
    .id;
    post_graphql::<mutations::FunctionUpdate, _>(
        &client,
        configs.get_backboard(),
        mutations::function_update::Variables {
            service_id: service.clone(),
            environment_id: environment.node.id.clone(),
            sleep_application: Some(serverless),
            cron_schedule: cron.clone(),
            start_command: Some(cmd),
        },
    )
    .await?;
    let domain = if domain {
        post_graphql::<mutations::ServiceDomainCreate, _>(
            &client,
            configs.get_backboard(),
            mutations::service_domain_create::Variables {
                service_id: service.clone(),
                environment_id: environment.node.id.clone(),
            },
        )
        .await?;
        let r = post_graphql::<queries::Domains, _>(
            &client,
            configs.get_backboard(),
            queries::domains::Variables {
                environment_id: environment.node.id.clone(),
                service_id: service.clone(),
                project_id: project.id.clone(),
            },
        )
        .await?;
        let domain = r.domains.service_domains.first().unwrap();
        Some(domain.clone().domain)
    } else {
        None
    };
    success_spinner(&mut spinner, "Function created".into());
    if watch {
        let (watch_tx, mut file_change) = tokio::sync::mpsc::unbounded_channel();
        let o = watch_tx.clone();
        let mut watcher = RecommendedWatcher::new(
            move |res| {
                if let Err(e) = watch_tx.send(res) {
                    eprintln!("Failed to send file event: {}", e);
                }
            },
            Config::default(),
        )?;
        watcher.watch(&path, RecursiveMode::NonRecursive)?;
        o.send(Ok(Event {
            kind: notify::EventKind::Any,
            paths: vec![path.clone()],
            attrs: Default::default(),
        }))?;
        let mut info = formatdoc!(
            "
        Name: {}
        Project: {}
        Environment: {}
        Local file: {}
        ",
            name.blue(),
            project.name.blue(),
            environment.node.name.clone().blue(),
            &path.display().to_string().blue()
        );
        if let Some(domain) = domain {
            writedoc!(
                info,
                "
            Domain: {}{}
            ",
                "https://".blue(),
                domain.blue()
            )?
        }
        if let Some(cron) = &cron {
            writedoc!(
                info,
                "Cron: {} ({})",
                cron_descriptor::cronparser::cron_expression_descriptor::get_description_cron(cron)
                    .expect("cron is not valid")
                    .blue(),
                cron.blue()
            )?;
        }
        let ser = Arc::new(service.clone());
        let env = Arc::new(environment.node.id.clone());
        if terminal {
            clear()?;
            println!("{}", info);
        }

        let mut current_cancel_token: Option<CancellationToken> = None;

        loop {
            tokio::select! {
                event = file_change.recv() => {
                    match event {
                        Some(Ok(_e)) => {
                            // Cancel any existing deployment stream
                            if let Some(token) = current_cancel_token.take() {
                                token.cancel();
                            }
                            let mut spinner = create_spinner("Updating function".into());
                            let cmd = get_start_cmd(&path)?;
                            if cmd.len() > 96 * 1024 {
                                println!("{}: Your function is too large", "ERROR".red());
                                continue;
                            }

                            let configs = Configs::new()?;
                            let client = GQLClient::new_authorized(&configs)?;

                            post_graphql::<mutations::FunctionUpdate, _>(&client, configs.get_backboard(), mutations::function_update::Variables {
                                service_id: (*ser).clone(),
                                environment_id: (*env).clone(),
                                sleep_application: None,
                                cron_schedule: None,
                                start_command: Some(cmd),
                            }).await?;

                            let latest = post_graphql::<mutations::ServiceInstanceDeploy, _>(&client, configs.get_backboard(), mutations::service_instance_deploy::Variables {
                                environment_id: (*env).clone(),
                                service_id: (*ser).clone()
                            }).await?.service_instance_deploy_v2;

                            let stream = subscribe_graphql::<subscriptions::Deployment>(subscriptions::deployment::Variables {
                                id: latest
                            }).await?;
                            success_spinner(&mut spinner, "Function updated".into());

                            // Create new cancellation token
                            let cancel_token = CancellationToken::new();
                            current_cancel_token = Some(cancel_token.clone());

                            // Spawn task to handle this stream
                            let info_clone = info.clone();
                            tokio::spawn(async move {
                                tokio::pin!(stream);
                                loop {
                                    tokio::select! {
                                        stream_item = stream.next() => {
                                            match stream_item {
                                                Some(Ok(stream_data)) => {
                                                    if let Some(data) = stream_data.data {
                                                        let deployment = data.deployment;
                                                        if terminal {
                                                            clear().ok();
                                                            let mut info = info_clone.clone();
                                                            let status = serde_json::to_string(&deployment.status)
                                                                .expect("failed to serialize deployment status")
                                                                .replace('"', "");
                                                            writedoc!(&mut info, "Latest deployment: {} ({})",
                                                                match deployment.status {
                                                                    DeploymentStatus::BUILDING | DeploymentStatus::DEPLOYING |
                                                                    DeploymentStatus::INITIALIZING | DeploymentStatus::QUEUED => status.blue(),
                                                                    DeploymentStatus::CRASHED | DeploymentStatus::FAILED => status.red(),
                                                                    DeploymentStatus::SLEEPING => status.yellow(),
                                                                    DeploymentStatus::SUCCESS => status.green(),
                                                                    _ => status.dimmed(),
                                                                },
                                                                chrono::Local::now().format("%I:%M:%S %p").to_string().dimmed(),
                                                            ).expect("failed to write");
                                                            println!("{}", info);
                                                        }
                                                    }
                                                }
                                                Some(Err(_)) | None => break,
                                            }
                                        }
                                        _ = cancel_token.cancelled() => {
                                            // Stream cancelled, exit

                                            break;
                                        }
                                    }
                                }
                            });
                        },
                        Some(Err(_e)) => continue,
                        None => break,
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
    }
    Ok(())
}

fn get_start_cmd(path: &PathBuf) -> Result<String> {
    Ok(format!(
        "./run.sh {}",
        BASE64_STANDARD.encode(std::fs::read(path)?)
    ))
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
        bail!("Path provided does not exist");
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
