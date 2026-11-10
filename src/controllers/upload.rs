use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result};
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

/// A project directory bundled for upload.
pub struct DeployTarball {
    /// The gzipped tar archive.
    pub bytes: Vec<u8>,
    /// Symlinks left out of the archive because they resolve to somewhere
    /// outside the archive root, relative to that root.
    pub skipped_symlinks: Vec<PathBuf>,
}

/// Create a gzipped tarball from a project directory, respecting .railwayignore and .gitignore.
///
/// Symlinks are followed, so a link to a sibling package in a monorepo is
/// bundled as the files it points to. A link whose resolved target lies
/// outside `archive_prefix_path` is not part of the project and is left out
/// of the archive; such links are listed in
/// [`DeployTarball::skipped_symlinks`] so the user can see what was omitted.
///
/// `on_progress` is called with `(current, total)` after each entry is processed.
/// The first call is `(0, total)` once indexing is complete (before compression begins).
pub fn create_deploy_tarball(
    project_path: &Path,
    archive_prefix_path: &Path,
    no_gitignore: bool,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<DeployTarball> {
    let bytes = Vec::<u8>::new();
    let arc = Arc::new(Mutex::new(bytes));
    let mut parz = ZBuilder::<Gzip, _>::new()
        .num_threads(num_cpus::get())
        .from_writer(SynchronizedWriter::new(arc.clone()));

    let ignore_paths = [".git", "node_modules"];
    let ignore_paths: Vec<&std::ffi::OsStr> =
        ignore_paths.iter().map(std::ffi::OsStr::new).collect();

    // The boundary symlink targets are checked against. Canonical, so that
    // `/tmp` vs `/private/tmp` style aliases of the root compare equal to
    // the canonical targets below.
    let archive_root = archive_prefix_path.canonicalize().with_context(|| {
        format!(
            "Failed to resolve project directory {}",
            archive_prefix_path.display()
        )
    })?;
    let skipped_symlinks = Arc::new(Mutex::new(Vec::<PathBuf>::new()));

    {
        let mut archive = Builder::new(&mut parz);
        let mut builder = WalkBuilder::new(project_path);
        builder.add_custom_ignore_filename(".railwayignore");
        if no_gitignore {
            builder.git_ignore(false);
        }

        // Applied to every entry before the walker descends into it and
        // before the tar builder opens it. The tar entry name is derived from
        // the lexical path, so the project-root check has to be made on the
        // resolved target instead.
        let skipped = Arc::clone(&skipped_symlinks);
        let prefix = archive_prefix_path.to_path_buf();
        builder.filter_entry(move |entry| {
            if !entry.path_is_symlink() || symlink_stays_within(entry.path(), &archive_root) {
                return true;
            }
            let relative = entry
                .path()
                .strip_prefix(&prefix)
                .unwrap_or(entry.path())
                .to_path_buf();
            skipped
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(relative);
            false
        });

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

    let bytes = Arc::try_unwrap(arc)
        .map_err(|_| {
            anyhow::anyhow!("internal error: tarball buffer still has references after compression")
        })?
        .into_inner()
        .map_err(|e| anyhow::anyhow!("internal error: failed to unwrap tarball buffer: {e}"))?;
    let skipped_symlinks = Arc::try_unwrap(skipped_symlinks)
        .map_err(|_| {
            anyhow::anyhow!("internal error: walker still holds the skipped-symlink list")
        })?
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    Ok(DeployTarball {
        bytes,
        skipped_symlinks,
    })
}

/// Whether the symlink at `link` ultimately resolves to a path under
/// `archive_root` (itself canonical). A dangling link resolves nowhere and
/// so is treated as outside.
fn symlink_stays_within(link: &Path, archive_root: &Path) -> bool {
    link.canonicalize()
        .map(|target| target.starts_with(archive_root))
        .unwrap_or(false)
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
    let mut request = client
        .post(url.to_string())
        .header("Content-Type", "application/gzip");

    // Agent attribution: present only when an agent harness is driving the
    // CLI. Backboard threads these into the deployment-create event so an
    // agentic `up` deploy is distinguishable from a human CLI deploy
    // (`source = "CLI Agent"`), closing the signup → deploy funnel.
    if let Some((caller, agent_session_id)) = crate::telemetry::deploy_attribution() {
        request = request.header("x-railway-caller", caller);
        if let Some(agent_session_id) = agent_session_id {
            request = request.header("x-railway-agent-session", agent_session_id);
        }
    }

    let res = request.body(body).send().await?;

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

    let mut response = res.json::<UpResponse>().await?;
    // Backboard can leave an empty query parameter at the end of these URLs.
    for url in [&mut response.url, &mut response.logs_url] {
        *url = url.trim_end_matches('&').to_owned();
    }
    Ok(response)
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Read;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use super::create_deploy_tarball;

    /// Entry name → contents for every entry in the gzipped tarball. The tar
    /// reader normalises the `./` prefix the builder writes away.
    fn entries(tarball: &[u8]) -> BTreeMap<String, Vec<u8>> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
        let mut out = BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).unwrap();
            out.insert(name, contents);
        }
        out
    }

    fn bundle(project: &Path, root: &Path) -> (BTreeMap<String, Vec<u8>>, Vec<String>) {
        let tarball = create_deploy_tarball(project, root, false, |_, _| {}).unwrap();
        let skipped = tarball
            .skipped_symlinks
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        (entries(&tarball.bytes), skipped)
    }

    #[test]
    fn file_symlink_outside_root_is_not_bundled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(root.join("app.txt"), "app").unwrap();
        fs::write(outside.join("other.txt"), "outside").unwrap();
        symlink(outside.join("other.txt"), root.join("link.txt")).unwrap();

        let (files, skipped) = bundle(&root, &root);

        assert_eq!(files.get("app.txt").unwrap(), b"app");
        assert!(!files.contains_key("link.txt"), "{files:?}");
        assert!(files.values().all(|c| c != b"outside"), "{files:?}");
        assert_eq!(skipped, vec!["link.txt"]);
    }

    #[test]
    fn directory_symlink_outside_root_is_not_descended() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(outside.join("nested")).unwrap();
        fs::write(outside.join("nested").join("other.txt"), "outside").unwrap();
        symlink(&outside, root.join("linkdir")).unwrap();

        let (files, skipped) = bundle(&root, &root);

        assert!(files.keys().all(|k| !k.starts_with("linkdir")), "{files:?}");
        assert_eq!(skipped, vec!["linkdir"]);
    }

    /// A link to a link is judged on the final target.
    #[test]
    fn chained_symlink_is_judged_on_its_final_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("other.txt"), "outside").unwrap();
        symlink(outside.join("other.txt"), root.join("inner")).unwrap();
        symlink("inner", root.join("hop")).unwrap();

        let (files, mut skipped) = bundle(&root, &root);

        assert!(files.values().all(|c| c != b"outside"), "{files:?}");
        skipped.sort();
        assert_eq!(skipped, vec!["hop", "inner"]);
    }

    /// Links that stay inside the project keep working as they always have:
    /// dereferenced into the archive.
    #[test]
    fn symlink_within_root_is_still_bundled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::write(root.join("lib").join("real.txt"), "real").unwrap();
        symlink("lib/real.txt", root.join("alias.txt")).unwrap();
        symlink("lib", root.join("libdir")).unwrap();

        let (files, skipped) = bundle(&root, &root);

        assert_eq!(files.get("alias.txt").unwrap(), b"real");
        assert_eq!(files.get("libdir/real.txt").unwrap(), b"real");
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    /// `railway up --path app` in a monorepo: a link from the service
    /// directory to a sibling package is inside the archive root even
    /// though it leaves the walked directory.
    #[test]
    fn symlink_to_sibling_under_archive_root_is_bundled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir_all(root.join("shared")).unwrap();
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(root.join("shared").join("lib.txt"), "lib").unwrap();
        symlink("../shared", root.join("app").join("shared")).unwrap();
        // ...but a link from the same place to outside the root is not.
        fs::write(tmp.path().join("other.txt"), "outside").unwrap();
        symlink("../../other.txt", root.join("app").join("other.txt")).unwrap();

        let (files, skipped) = bundle(&root.join("app"), &root);

        assert_eq!(files.get("app/shared/lib.txt").unwrap(), b"lib");
        assert!(files.values().all(|c| c != b"outside"), "{files:?}");
        assert_eq!(skipped, vec!["app/other.txt"]);
    }

    /// The root may itself be reached through a symlink (macOS `/tmp` is
    /// `/private/tmp`); in-root links must still compare as in-root.
    #[test]
    fn archive_root_given_through_symlink_compares_canonically() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let alias = tmp.path().join("alias");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("real.txt"), "real").unwrap();
        symlink("real.txt", root.join("link.txt")).unwrap();
        symlink(&root, &alias).unwrap();

        let (files, skipped) = bundle(&alias, &alias);

        assert_eq!(files.get("link.txt").unwrap(), b"real");
        assert!(skipped.is_empty(), "{skipped:?}");
    }
}
