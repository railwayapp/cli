use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, bail};

use futures::StreamExt;
use gzp::{ZBuilder, deflate::Gzip};
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressFinish, ProgressIterator, ProgressStyle};
use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};
use synchronized_writer::SynchronizedWriter;
use tar::Builder;

use crate::{
    consts::TICK_STRING,
    controllers::{
        deployment::{stream_build_logs, stream_deploy_logs},
        environment::get_matched_environment,
        project::get_project,
        service::get_or_prompt_service,
    },
    errors::RailwayError,
    subscription::subscribe_graphql,
    subscriptions::deployment::DeploymentStatus,
    util::logs::print_log,
};

use super::*;

/// Upload and deploy project from the current directory
#[derive(Parser)]
pub struct Args {
    path: Option<PathBuf>,

    #[clap(short, long)]
    /// Don't attach to the log stream
    detach: bool,

    #[clap(short, long)]
    /// Stream build logs only, then exit (equivalent to setting $CI=true).
    ci: bool,

    #[clap(short, long)]
    /// Service to deploy to (defaults to linked service)
    service: Option<String>,

    #[clap(short, long)]
    /// Environment to deploy to (defaults to linked environment)
    environment: Option<String>,

    #[clap(long)]
    /// Don't ignore paths from .gitignore
    no_gitignore: bool,

    #[clap(long)]
    /// Use the path argument as the prefix for the archive instead of the project directory.
    path_as_root: bool,

    #[clap(long)]
    /// Verbose output
    verbose: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpResponse {
    pub deployment_id: String,
    pub url: String,
    pub logs_url: String,
    pub deployment_domain: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpErrorResponse {
    pub message: String,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let hostname = configs.get_host();
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let deploy_paths = get_deploy_paths(&args, &linked_project)?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());
    let environment_id = get_matched_environment(&project, environment)?.id;

    let service = get_or_prompt_service(linked_project.clone(), project, args.service).await?;

    let spinner = if std::io::stdout().is_terminal() {
        let spinner = ProgressBar::new_spinner()
            .with_style(
                ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg:.cyan.bold}")?,
            )
            .with_message("Indexing".to_string());
        spinner.enable_steady_tick(Duration::from_millis(100));
        Some(spinner)
    } else {
        println!("Indexing...");
        None
    };

    // Explanation for the below block
    // arc is a reference counted pointer to a mutexed vector of bytes, which
    // stores the actual tarball in memory.
    //
    // parz is a parallelized gzip writer, which writes to the arc (still in memory)
    //
    // archive is a tar archive builder, which writes to the parz writer `new(&mut parz)
    //
    // builder is a directory walker which returns an iterable that we loop over to add
    // files to the tarball (archive)
    //
    // during the iteration of `builder`, we ignore all files that match the patterns found in
    // .railwayignore
    // .gitignore
    // .git/**
    // node_modules/**
    let bytes = Vec::<u8>::new();
    let arc = Arc::new(Mutex::new(bytes));
    let mut parz = ZBuilder::<Gzip, _>::new()
        .num_threads(num_cpus::get())
        .from_writer(SynchronizedWriter::new(arc.clone()));

    // list of all paths to ignore by default
    let ignore_paths = [".git", "node_modules"];
    let ignore_paths: Vec<&std::ffi::OsStr> =
        ignore_paths.iter().map(std::ffi::OsStr::new).collect();

    {
        let mut archive = Builder::new(&mut parz);
        let mut builder = WalkBuilder::new(deploy_paths.project_path);
        builder.add_custom_ignore_filename(".railwayignore");
        if args.no_gitignore {
            builder.git_ignore(false);
        }

        let walker = builder.follow_links(true).hidden(false);
        let walked = walker.build().collect::<Vec<_>>();
        if let Some(spinner) = spinner {
            spinner.finish_with_message("Indexed");
        }
        if std::io::stdout().is_terminal() {
            let pg = ProgressBar::new(walked.len() as u64)
                .with_style(
                    ProgressStyle::default_bar()
                        .template("{spinner:.green} {msg:.cyan.bold} [{bar:20}] {percent}% ")?
                        .progress_chars("=> ")
                        .tick_chars(TICK_STRING),
                )
                .with_message("Compressing")
                .with_finish(ProgressFinish::WithMessage("Compressed".into()));
            pg.enable_steady_tick(Duration::from_millis(100));

            for entry in walked.into_iter().progress_with(pg) {
                let entry = entry?;
                let path = entry.path();
                if path
                    .components()
                    .any(|c| ignore_paths.contains(&c.as_os_str()))
                {
                    continue;
                }
                let stripped =
                    PathBuf::from(".").join(path.strip_prefix(&deploy_paths.archive_prefix_path)?);
                archive.append_path_with_name(path, stripped)?;
            }
        } else {
            for entry in walked.into_iter() {
                let entry = entry?;
                let path = entry.path();
                if path
                    .components()
                    .any(|c| ignore_paths.contains(&c.as_os_str()))
                {
                    continue;
                }
                let stripped =
                    PathBuf::from(".").join(path.strip_prefix(&deploy_paths.archive_prefix_path)?);
                archive.append_path_with_name(path, stripped)?;
            }
        }
    }
    parz.finish()?;

    let url = format!(
        "https://backboard.{hostname}/project/{}/environment/{}/up?serviceId={}",
        linked_project.project,
        environment_id,
        service.clone().unwrap_or_default(),
    );

    if args.verbose {
        let bytes_len = arc.lock().unwrap().len();
        println!("railway up");
        println!("service: {}", service.clone().unwrap_or_default());
        println!("environment: {environment_id}");
        println!("bytes: {bytes_len}");
        println!("url: {url}");
    }

    let builder = client.post(url);
    let spinner = if std::io::stdout().is_terminal() {
        let spinner = ProgressBar::new_spinner()
            .with_style(
                ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg:.cyan.bold}")?,
            )
            .with_message("Uploading");
        spinner.enable_steady_tick(Duration::from_millis(100));
        Some(spinner)
    } else {
        println!("Uploading...");
        None
    };

    let body = arc.lock().unwrap().clone();

    let res = builder
        .header("Content-Type", "multipart/form-data")
        .body(body)
        .send()
        .await?;

    let status = res.status();
    if status != 200 {
        if let Some(spinner) = spinner {
            spinner.finish_with_message("Failed");
        }

        // If a user error, parse the response
        if status == 400 {
            let body = res.json::<UpErrorResponse>().await?;
            return Err(RailwayError::FailedToUpload(body.message).into());
        }

        if status == 413 {
            let err = res.text().await?;
            let filesize = arc.lock().unwrap().len();
            return Err(RailwayError::FailedToUpload(format!(
                "Failed to upload code. File too large ({filesize} bytes): {err}",
            )))?;
        }

        return Err(RailwayError::FailedToUpload(format!(
            "Failed to upload code with status code {status}"
        ))
        .into());
    }

    let body = res.json::<UpResponse>().await?;
    if let Some(spinner) = spinner {
        spinner.finish_with_message("Uploaded");
    }

    let deployment_id = body.deployment_id;

    println!("  {}: {}", "Build Logs".green().bold(), body.logs_url);

    if args.detach {
        return Ok(());
    }

    let ci_mode = Configs::env_is_ci() || args.ci;
    if ci_mode {
        println!("{}", "CI mode enabled".green().bold());
    }

    // If the user is not in a terminal AND if we are not in CI mode, don't stream logs
    if !std::io::stdout().is_terminal() && !ci_mode {
        return Ok(());
    }

    //	Create vector of log streaming tasks
    //	Always stream build logs
    let build_deployment_id = deployment_id.clone();
    let mut tasks = vec![tokio::task::spawn(async move {
        if let Err(e) = stream_build_logs(build_deployment_id, None, |log| {
            println!("{}", log.message);
            if args.ci && log.message.starts_with("No changed files matched patterns") {
                std::process::exit(0);
            }
        })
        .await
        {
            eprintln!("Failed to stream build logs: {e}");

            if ci_mode {
                std::process::exit(1);
            }
        }
    })];

    // Stream deploy logs only if is not in ci mode
    if !ci_mode {
        let deploy_deployment_id = deployment_id.clone();
        tasks.push(tokio::task::spawn(async move {
            if let Err(e) = stream_deploy_logs(deploy_deployment_id, None, |log| {
                print_log(log, false, true)
            })
            .await
            {
                eprintln!("Failed to stream deploy logs: {e}");
            }
        }));
    }

    let mut stream =
        subscribe_graphql::<subscriptions::Deployment>(subscriptions::deployment::Variables {
            id: deployment_id.clone(),
        })
        .await?;

    tokio::task::spawn(async move {
        while let Some(Ok(res)) = stream.next().await {
            if let Some(errors) = res.errors {
                eprintln!(
                    "Failed to get deploy status: {}",
                    errors
                        .iter()
                        .map(|err| err.to_string())
                        .collect::<Vec<String>>()
                        .join("; ")
                );
                if ci_mode {
                    std::process::exit(1);
                }
            }
            if let Some(data) = res.data {
                match data.deployment.status {
                    DeploymentStatus::SUCCESS => {
                        println!("{}", "Deploy complete".green().bold());
                        if ci_mode {
                            std::process::exit(0);
                        }
                    }
                    DeploymentStatus::FAILED => {
                        println!("{}", "Deploy failed".red().bold());
                        std::process::exit(1);
                    }
                    DeploymentStatus::CRASHED => {
                        println!("{}", "Deploy crashed".red().bold());
                        std::process::exit(1);
                    }
                    _ => {}
                }
            }
        }
    });

    futures::future::join_all(tasks).await;

    Ok(())
}

struct DeployPaths {
    project_path: PathBuf,
    archive_prefix_path: PathBuf,
}

fn get_deploy_paths(args: &Args, linked_project: &LinkedProject) -> Result<DeployPaths> {
    if args.path_as_root {
        if args.path.is_none() {
            bail!("--path-as-root requires a path to be specified");
        }

        let path = args.path.clone().unwrap();
        Ok(DeployPaths {
            project_path: path.clone(),
            archive_prefix_path: path,
        })
    } else {
        let project_dir: PathBuf = linked_project.project_path.clone().into();
        let project_path = match args.path {
            Some(ref path) => path.clone(),
            None => project_dir.clone(),
        };
        Ok(DeployPaths {
            project_path,
            archive_prefix_path: project_dir,
        })
    }
}
