use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use gzp::{ZBuilder, deflate::Gzip};
use ignore::WalkBuilder;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use synchronized_writer::SynchronizedWriter;
use tar::Builder;
use url::Url;

use crate::errors::RailwayError;

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

/// Create a gzipped tarball from a project directory, respecting .railwayignore and .gitignore.
///
/// `on_progress` is called with `(current, total)` after each entry is processed.
/// The first call is `(0, total)` once indexing is complete (before compression begins).
pub fn create_deploy_tarball(
    project_path: &Path,
    archive_prefix_path: &Path,
    no_gitignore: bool,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<Vec<u8>> {
    let bytes = Vec::<u8>::new();
    let arc = Arc::new(Mutex::new(bytes));
    let mut parz = ZBuilder::<Gzip, _>::new()
        .num_threads(num_cpus::get())
        .from_writer(SynchronizedWriter::new(arc.clone()));

    let ignore_paths = [".git", "node_modules"];
    let ignore_paths: Vec<&std::ffi::OsStr> =
        ignore_paths.iter().map(std::ffi::OsStr::new).collect();

    {
        let mut archive = Builder::new(&mut parz);
        let mut builder = WalkBuilder::new(project_path);
        builder.add_custom_ignore_filename(".railwayignore");
        if no_gitignore {
            builder.git_ignore(false);
        }

        let walker = builder.follow_links(true).hidden(false);
        let walked = walker.build().collect::<Vec<_>>();
        let total = walked.len();
        on_progress(0, total);

        for (i, entry) in walked.into_iter().enumerate() {
            let entry = entry?;
            let path = entry.path();
            if path
                .components()
                .any(|c| ignore_paths.contains(&c.as_os_str()))
            {
                continue;
            }
            let stripped =
                std::path::PathBuf::from(".").join(path.strip_prefix(archive_prefix_path)?);
            archive.append_path_with_name(path, stripped)?;
            on_progress(i + 1, total);
        }
    }
    parz.finish()?;

    let body = Arc::try_unwrap(arc)
        .map_err(|_| {
            anyhow::anyhow!("internal error: tarball buffer still has references after compression")
        })?
        .into_inner()
        .map_err(|e| anyhow::anyhow!("internal error: failed to unwrap tarball buffer: {e}"))?;
    Ok(body)
}

/// Upload a deploy tarball to Railway's backboard API.
pub async fn upload_deploy_tarball(
    client: &Client,
    hostname: &str,
    project_id: &str,
    environment_id: &str,
    service_id: Option<&str>,
    message: Option<&str>,
    body: Vec<u8>,
) -> Result<UpResponse> {
    let mut url = Url::parse(&format!(
        "https://backboard.{hostname}/project/{project_id}/environment/{environment_id}/up",
    ))?;

    url.query_pairs_mut()
        .append_pair("serviceId", service_id.unwrap_or_default());

    if let Some(message) = message {
        url.query_pairs_mut().append_pair("message", message);
    }

    let body_len = body.len();
    let res = client
        .post(url.to_string())
        .header("Content-Type", "application/gzip")
        .body(body)
        .send()
        .await?;

    let status = res.status();
    if status != 200 {
        if status == 400 {
            let body = res.json::<UpErrorResponse>().await?;
            return Err(RailwayError::FailedToUpload(body.message).into());
        }

        if status == 413 {
            let err = res.text().await?;
            return Err(RailwayError::FailedToUpload(format!(
                "Failed to upload code. File too large ({body_len} bytes): {err}",
            ))
            .into());
        }

        return Err(RailwayError::FailedToUpload(format!(
            "Failed to upload code with status code {status}"
        ))
        .into());
    }

    let response = res.json::<UpResponse>().await?;
    Ok(response)
}
