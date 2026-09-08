use std::path::Path;
use std::sync::{Arc, Mutex};

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

/// A walk error that means "this entry has no contents to upload", rather than a
/// condition that should stop the upload.
///
/// `follow_links(true)` makes the walker stat a symlink's target before the ignore
/// rules are consulted for that entry, so a dangling link surfaces here as
/// `NotFound` — the reason an exact-path `.railwayignore` rule cannot exclude one.
/// A file deleted mid-walk by a concurrent build lands here too; both are
/// legitimately skippable.
///
/// Only `NotFound` qualifies. A permission or genuine I/O error on a real entry
/// would mean a silently incomplete tarball, which is worth failing the deploy over.
fn is_missing_path(err: &ignore::Error) -> bool {
    err.io_error()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
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
    // The root reaches the walker as the same `NotFound` that `is_missing_path`
    // skips, and no caller validates it — so a typo'd path would upload an empty
    // but *successful* deploy. `metadata` follows symlinks, rejecting a dangling
    // root link too. Existence is all that is checked: `up <file>` is supported.
    std::fs::metadata(project_path).with_context(|| {
        format!(
            "Failed to read `{}` for upload: no such path",
            project_path.display()
        )
    })?;

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
            let entry = match entry {
                Ok(entry) => entry,
                // Skipping beats aborting the whole upload: see `is_missing_path`.
                Err(err) if is_missing_path(&err) => {
                    // Prefer the bare path: the error's own Display already embeds
                    // it inside an io description, so interpolating the error would
                    // print the path twice.
                    let what = match &err {
                        ignore::Error::WithPath { path, .. } => path.display().to_string(),
                        other => other.to_string(),
                    };
                    crate::util::reporter::warn(
                        "UNREADABLE_PATH_SKIPPED",
                        format!("skipping unreadable path: {what}"),
                        Some("a broken symlink, or a file removed while indexing"),
                    );
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
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

    let response = res.json::<UpResponse>().await?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sorted archive member names, so tests assert on what was actually
    /// captured rather than merely on success. Only the symlink cases inspect
    /// contents, and those are Unix-only.
    #[cfg(unix)]
    fn entries_of(root: &Path) -> Vec<String> {
        use std::io::Read;

        let tarball = tarball_of(root).expect("tarball");
        let mut decoder = flate2::read::GzDecoder::new(tarball.as_slice());
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        let mut archive = tar::Archive::new(decompressed.as_slice());
        let mut names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().display().to_string())
            .collect();
        names.sort();
        names
    }

    /// The fallible form, for the tests that assert the upload is refused.
    fn tarball_of(root: &Path) -> Result<Vec<u8>> {
        create_deploy_tarball(root, root, false, |_, _| {})
    }

    #[cfg(unix)]
    fn dangling(path: &Path) {
        std::os::unix::fs::symlink("../does-not-exist", path).unwrap();
    }

    /// A dangling symlink anywhere in the tree used to abort the whole upload with
    /// `No such file or directory`. Links are skipped; every real file still ships.
    #[cfg(unix)]
    #[test]
    fn dangling_symlinks_are_skipped_instead_of_failing_the_upload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("main.py"), b"print('hi')").unwrap();
        for sub in ["backend", "worker"] {
            std::fs::create_dir(root.join(sub)).unwrap();
            std::fs::write(root.join(sub).join("real.txt"), b"x").unwrap();
            dangling(&root.join(sub).join("AGENTS.md"));
        }

        let entries = entries_of(root);
        assert!(
            entries.iter().any(|name| name.ends_with("main.py")),
            "real files must still be uploaded, got {entries:?}"
        );
        assert_eq!(
            entries.iter().filter(|n| n.ends_with("real.txt")).count(),
            2,
            "files beside a broken link must survive, got {entries:?}"
        );
        assert!(
            !entries.iter().any(|name| name.ends_with("AGENTS.md")),
            "unresolvable symlinks must not be in the tarball, got {entries:?}"
        );
    }

    /// The reported CI shape: an exact-path `.railwayignore` rule alongside the
    /// dangling link. Asserts the rule is honored for a real file too, so this
    /// covers the ignore path and not just the skip branch.
    #[cfg(unix)]
    #[test]
    fn railwayignore_rules_apply_when_a_dangling_symlink_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("backend")).unwrap();
        std::fs::write(root.join("main.py"), b"print('hi')").unwrap();
        std::fs::write(root.join("backend/secret.txt"), b"nope").unwrap();
        dangling(&root.join("backend/AGENTS.md"));
        std::fs::write(
            root.join(".railwayignore"),
            b"/backend/AGENTS.md\n/backend/secret.txt\n",
        )
        .unwrap();

        let entries = entries_of(root);
        assert!(entries.iter().any(|name| name.ends_with("main.py")));
        assert!(
            !entries.iter().any(|name| name.ends_with("secret.txt")),
            "an ignored real file must be excluded, got {entries:?}"
        );
        assert!(!entries.iter().any(|name| name.ends_with("AGENTS.md")));
    }

    /// A symlink to a real file must still be followed and uploaded, and a
    /// symlinked directory still traversed — `follow_links(false)` would have been
    /// the tempting fix and would regress both (see #296).
    #[cfg(unix)]
    #[test]
    fn resolvable_symlinks_are_still_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("real.txt"), b"content").unwrap();
        std::os::unix::fs::symlink("real.txt", root.join("link.txt")).unwrap();
        std::fs::create_dir(root.join("realdir")).unwrap();
        std::fs::write(root.join("realdir/inner.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("realdir", root.join("linkdir")).unwrap();

        let entries = entries_of(root);
        assert!(
            entries.iter().any(|name| name.ends_with("link.txt")),
            "symlink to a real file must be uploaded, got {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|n| n.contains("linkdir") && n.ends_with("inner.txt")),
            "symlinked directory must be traversed, got {entries:?}"
        );
    }

    /// A symlink loop reports ELOOP, not `NotFound`, so the skip must not swallow it.
    #[cfg(unix)]
    #[test]
    fn symlink_loop_stays_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("keep.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(root.join("loop_a"), root.join("loop_b")).unwrap();
        std::os::unix::fs::symlink(root.join("loop_b"), root.join("loop_a")).unwrap();
        assert!(
            tarball_of(root).is_err(),
            "a symlink loop is not a broken link and must stay fatal"
        );
    }

    /// A missing root reaches the walker as the same `NotFound` the skip branch
    /// swallows, so without an explicit check `up ./typo` would deploy nothing and
    /// report success.
    #[test]
    fn missing_root_must_be_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = tarball_of(&dir.path().join("does-not-exist"))
            .expect_err("a missing root must not deploy successfully")
            .to_string();
        assert!(
            err.contains("no such path"),
            "error should name the missing path, got: {err}"
        );
    }

    /// Same trap via a root that exists as a link but does not resolve — this is
    /// what breaks if the root check is switched to `symlink_metadata`.
    #[cfg(unix)]
    #[test]
    fn dangling_symlink_as_the_root_must_be_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("rootlink");
        std::os::unix::fs::symlink("nowhere", &root).unwrap();
        assert!(
            tarball_of(&root).is_err(),
            "a dangling root symlink must not produce a successful empty deploy"
        );
    }

    /// The root check must not over-correct: an empty directory is legitimately
    /// empty, and `up <file>` is a supported shape.
    #[test]
    fn empty_directory_and_single_file_roots_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        tarball_of(dir.path()).expect("an empty project directory is not an error");

        let file = dir.path().join("solo.txt");
        std::fs::write(&file, b"x").unwrap();
        create_deploy_tarball(&file, dir.path(), false, |_, _| {})
            .expect("a file root must still be uploadable");
    }
}
