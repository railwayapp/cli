use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::bail;

use gzp::{deflate::Gzip, ZBuilder};
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressFinish, ProgressIterator, ProgressStyle};
use is_terminal::IsTerminal;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use synchronized_writer::SynchronizedWriter;
use tar::Builder;

use crate::{
    commands::queries::deployment::DeploymentStatus,
    consts::TICK_STRING,
    controllers::{
        deployment::{get_deployment, stream_build_logs, stream_deploy_logs},
        environment::get_matched_environment,
        project::get_project,
    },
    errors::RailwayError,
    util::prompt::{prompt_select, PromptService},
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
    /// Service to deploy to (defaults to linked service)
    service: Option<String>,

    #[clap(short, long)]
    /// Environment to deploy to (defaults to linked environment)
    environment: Option<String>,
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

pub async fn get_service_to_deploy(
    configs: &Configs,
    client: &Client,
    service_arg: Option<String>,
) -> Result<Option<String>> {
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(client, configs, linked_project.project.clone()).await?;
    let services = project.services.edges.iter().collect::<Vec<_>>();

    let service = if let Some(service_arg) = service_arg {
        // If the user specified a service, use that
        let service_id = services
            .iter()
            .find(|service| service.node.name == service_arg || service.node.id == service_arg);
        if let Some(service_id) = service_id {
            Some(service_id.node.id.to_owned())
        } else {
            bail!("Service not found");
        }
    } else if let Some(service) = linked_project.service {
        // If the user didn't specify a service, but we have a linked service, use that
        Some(service)
    } else {
        // If the user didn't specify a service, and we don't have a linked service, get the first service

        if services.is_empty() {
            // If there are no services, backboard will generate one for us
            None
        } else {
            // If there are multiple services, prompt the user to select one
            if std::io::stdout().is_terminal() {
                let prompt_services: Vec<_> =
                    services.iter().map(|s| PromptService(&s.node)).collect();
                let service = prompt_select("Select a service to deploy to", prompt_services)?;
                Some(service.0.id.clone())
            } else {
                bail!("Multiple services found. Please specify a service to deploy to.")
            }
        }
    };
    Ok(service)
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let hostname = configs.get_host();
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let prefix: PathBuf = configs.get_closest_linked_project_directory()?.into();

    let path = match args.path {
        Some(path) => path,
        None => prefix.clone(),
    };

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());
    let environment_id = get_matched_environment(&project, environment)?.id;

    let service = get_service_to_deploy(&configs, &client, args.service).await?;

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
        let mut builder = WalkBuilder::new(path);
        builder.add_custom_ignore_filename(".railwayignore");
        builder.add_custom_ignore_filename(".gitignore");

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
                let stripped = PathBuf::from(".").join(path.strip_prefix(&prefix)?);
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
                let stripped = PathBuf::from(".").join(path.strip_prefix(&prefix)?);
                archive.append_path_with_name(path, stripped)?;
            }
        }
    }
    parz.finish()?;

    let builder = client.post(format!(
        "https://backboard.{hostname}/project/{}/environment/{}/up?serviceId={}",
        linked_project.project,
        environment_id,
        service.unwrap_or_default(),
    ));
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
            let filesize = arc.lock().unwrap().len();
            return Err(RailwayError::FailedToUpload(format!(
                "Failed to upload code. File too large ({} bytes)",
                filesize
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

    // If the user is not in a terminal, don't stream logs
    if !std::io::stdout().is_terminal() {
        return Ok(());
    }

    // Stream both build and deploy logs
    let build_deployment_id = deployment_id.clone();
    let deploy_deployment_id = deployment_id.clone();

    let tasks = vec![
        tokio::task::spawn(async move {
            if let Err(e) =
                stream_build_logs(build_deployment_id, |log| println!("{}", log.message)).await
            {
                eprintln!("Failed to stream build logs: {}", e);
            }
        }),
        tokio::task::spawn(async move {
            if let Err(e) =
                stream_deploy_logs(deploy_deployment_id, |log| println!("{}", log.message)).await
            {
                eprintln!("Failed to stream deploy logs: {}", e);
            }
        }),
    ];

    // If the build fails, we want to terminate the process
    tokio::task::spawn(async move {
        match wait_for_failure(deployment_id.clone()).await {
            Ok(_) => {
                println!("{}", "Build failed".red().bold());
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Failed to fetch deployment status: {}", e);
            }
        }
    });

    futures::future::join_all(tasks).await;

    Ok(())
}

async fn wait_for_failure(deployment_id: String) -> Result<(), anyhow::Error> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;

        if let Ok(deployment) = get_deployment(&client, &configs, deployment_id.clone()).await {
            if deployment.status == DeploymentStatus::FAILED {
                break;
            }
        }
    }

    Ok(())
}
