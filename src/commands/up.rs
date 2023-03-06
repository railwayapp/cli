use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::bail;
use futures::StreamExt;
use gzp::{deflate::Gzip, ZBuilder};
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressFinish, ProgressIterator, ProgressStyle};
use is_terminal::IsTerminal;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use synchronized_writer::SynchronizedWriter;
use tar::Builder;

use crate::{
    consts::TICK_STRING,
    subscription::subscribe_graphql,
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
}

pub async fn get_service_to_deploy(
    configs: &Configs,
    client: &Client,
    service_arg: Option<String>,
) -> Result<Option<String>> {
    let linked_project = configs.get_linked_project().await?;

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };
    let res = post_graphql::<queries::Project, _>(client, configs.get_backboard(), vars).await?;
    let body = res.data.context("Failed to get project (query project)")?;

    let services = body.project.services.edges.iter().collect::<Vec<_>>();

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
        } else if services.len() == 1 {
            // If there is only one service, use that
            services.first().map(|service| service.node.id.to_owned())
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
    let bytes = Vec::<u8>::new();
    let arc = Arc::new(Mutex::new(bytes));
    let mut parz = ZBuilder::<Gzip, _>::new()
        .num_threads(num_cpus::get())
        .from_writer(SynchronizedWriter::new(arc.clone()));
    {
        let mut archive = Builder::new(&mut parz);
        let mut builder = WalkBuilder::new(args.path.unwrap_or_else(|| ".".into()));
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
                archive.append_path(entry?.path())?;
            }
        } else {
            for entry in walked.into_iter() {
                archive.append_path(entry?.path())?;
            }
        }
    }
    parz.finish()?;

    let builder = client.post(format!(
        "https://backboard.{hostname}/project/{}/environment/{}/up?serviceId={}",
        linked_project.project,
        linked_project.environment,
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
        .await?
        .error_for_status()?;

    let body = res.json::<UpResponse>().await?;
    if let Some(spinner) = spinner {
        spinner.finish_with_message("Uploaded");
    }
    println!("  {}: {}", "Build Logs".green().bold(), body.logs_url);
    if args.detach {
        return Ok(());
    }

    // If the user is not in a terminal, don't stream logs
    if !std::io::stdout().is_terminal() {
        return Ok(());
    }

    let vars = queries::deployments::Variables {
        project_id: linked_project.project.clone(),
    };

    let res =
        post_graphql::<queries::Deployments, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;

    let mut deployments: Vec<_> = body
        .project
        .deployments
        .edges
        .into_iter()
        .map(|deployment| deployment.node)
        .collect();
    deployments.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let latest_deployment = deployments.first().context("No deployments found")?;
    let vars = subscriptions::build_logs::Variables {
        deployment_id: latest_deployment.id.clone(),
        filter: Some(String::new()),
        limit: Some(500),
    };

    let (_client, mut log_stream) = subscribe_graphql::<subscriptions::BuildLogs>(vars).await?;
    while let Some(Ok(log)) = log_stream.next().await {
        let log = log.data.context("Failed to retrieve log")?;
        for line in log.build_logs {
            println!("{}", line.message);
        }
    }

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpResponse {
    pub url: String,
    pub logs_url: String,
    pub deployment_domain: String,
}
