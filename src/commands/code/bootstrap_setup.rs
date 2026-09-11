//! Create an expendable setup VM; only its named checkpoint survives success.
use super::*;
use crate::commands::cloud_agent::tui::bootstrap_setup::Request;
use crate::controllers::agent_bootstrap as bootstrap;

pub(crate) fn repository_url(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        bail!("Enter a repository URL or owner/repo.");
    }
    let candidate = if !value.contains("://") {
        let parts: Vec<_> = value.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|s| {
                s.is_empty()
                    || *s == "."
                    || *s == ".."
                    || !s
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            })
        {
            bail!("Use owner/repo or an HTTPS repository URL.");
        }
        format!("https://github.com/{value}")
    } else {
        value.to_owned()
    };
    let url = url::Url::parse(&candidate)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() == "/"
    {
        bail!("Use an HTTPS repository URL without embedded credentials, a query, or a fragment.");
    }
    Ok(url.to_string())
}

pub(crate) async fn create(req: Request, progress: &dyn Progress) -> Result<bootstrap::Bootstrap> {
    let repo = req.repo.as_deref().map(repository_url).transpose()?;
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    // These checks must never open a prompt underneath the form, or spend a VM
    // when the user cannot reach it. The normal launch flow handles enrollment.
    crate::commands::ssh::native::ensure_ssh_key_noninteractive(&client, &configs).await?;
    if req.harness == "claude" && claude_needs_local_mint() {
        progress.step("Preparing Claude sign-in");
        tokio::task::spawn_blocking(mint_claude_credential_headless).await??;
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    let files = config_files(&home, &req.harness)?;
    let url = configs.get_backboard();
    let launch_req = req.clone();
    create_with(
        &mut configs,
        &client,
        &url,
        &req,
        progress,
        |agent| async move {
            progress.step("Configuring the coding agent");
            let mut args = LaunchArgs::for_target(
                launch_req.target.project_id.clone(),
                launch_req.target.environment_id.clone(),
                &launch_req.harness,
                false,
                None,
                Some(agent.id),
            );
            args.app_mode = true;
            args.bootstrap_setup = true;
            args.client_on_agent = true;
            let prepared = prepare(&args, progress, SessionStyle::Pane).await?;
            progress.step("Copying harness settings");
            let repository = repo.clone();
            let clone_requested = repository.is_some();
            if clone_requested {
                progress.step("Cloning repository into /app");
            }
            tokio::task::spawn_blocking(move || {
                configure_disk(&prepared, files, repository.as_deref())
            })
            .await??;
            Ok(())
        },
    )
    .await
}

async fn create_with<F, Fut>(
    configs: &mut Configs,
    client: &reqwest::Client,
    url: &str,
    req: &Request,
    progress: &dyn Progress,
    configure: F,
) -> Result<bootstrap::Bootstrap>
where
    F: FnOnce(ca::Agent) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if bootstrap::list(configs, client, url, &req.target.environment_id)
        .await?
        .iter()
        .any(|b| b.name == req.name)
    {
        bail!(
            "A bootstrap named '{}' already exists. Choose a new name.",
            req.name
        );
    }
    progress.step("Creating setup VM");
    let agent = ca::create(
        client,
        url,
        &req.target.environment_id,
        Some(format!("bootstrap-setup-{}", rand::random::<u64>())),
        None,
        ca::CreateOptions::default(),
    )
    .await?;
    // Always retain the ID before provisioning so every later failure can clean
    // up the exact VM this operation created, never a user's existing agent.
    let result: Result<bootstrap::Bootstrap> = async {
        configure(agent.clone()).await?;
        progress.step("Saving checkpoint");
        let saved = bootstrap::save(client, url, &agent.id, &req.name, None, None).await?;
        let mut saved = bootstrap::wait_ready(client, url, saved).await?;
        progress.step("Setting local default");
        saved.is_default = configs
            .set_agent_bootstrap_default(&req.target.environment_id, &saved.id, false)
            .await?;
        Ok(saved)
    }
    .await;
    progress.step("Deleting setup VM");
    let cleanup = ca::delete(client, url, &agent.id).await;
    match (result, cleanup) {
        (Ok(saved), Ok(())) => {
            progress.step("Bootstrap ready — setup VM deleted");
            Ok(saved)
        }
        (Err(error), Ok(())) => {
            Err(error.context("Bootstrap setup failed; the setup VM was deleted"))
        }
        (result, Err(error)) => {
            let outcome = match result {
                Ok(_) => "Bootstrap saved and selected locally".to_string(),
                Err(e) => format!("Bootstrap setup failed: {e:#}"),
            };
            bail!(
                "{outcome}, but setup VM '{}' could not be deleted: {error:#}. Delete it with `railway ca delete {}`.",
                agent.name,
                agent.name
            )
        }
    }
}

fn config_files(home: &Path, harness: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let paths: &[&str] = match harness {
        "claude" => &[".claude/settings.json", ".claude/CLAUDE.md"],
        "codex" => &[".codex/config.toml", ".codex/AGENTS.md"],
        "grok" => &[".grok/config.toml"],
        "opencode" | "opencode2" => &[
            ".config/opencode/opencode.json",
            ".config/opencode/opencode.jsonc",
            ".config/opencode/AGENTS.md",
        ],
        "railway" => &[],
        _ => bail!("Unknown coding agent"),
    };
    let mut files = Vec::new();
    for relative in paths {
        let path = home.join(relative);
        match std::fs::read(&path) {
            Ok(data) if data.len() <= 1024 * 1024 => {
                merge_config(relative, &data, &[])
                    .with_context(|| format!("Invalid harness config {}", path.display()))?;
                files.push((relative.to_string(), data));
            }
            Ok(_) => bail!("Harness config {} exceeds 1 MiB", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(files)
}

fn configure_disk(
    prepared: &Prepared,
    files: Vec<(String, Vec<u8>)>,
    repo: Option<&str>,
) -> Result<()> {
    let mut relay = relay_ssh()?;
    relay.opts = prepared.relay_opts.clone();
    for (path, local) in files {
        // Keep platform-managed settings (especially Railway MCP and relay
        // routes) when merging the local user's preferences into the VM.
        let remote_path = format!("$HOME/{}", path);
        let remote = ssh_plumbing(
            &prepared.ssh_target,
            &format!("if [ -f \"{remote_path}\" ]; then cat \"{remote_path}\"; fi"),
            prepared.identity.as_deref(),
            None,
            &relay,
            None,
        )?;
        let data = merge_config(&path, &local, &remote)?;
        let parent = path.rsplit_once('/').unwrap().0;
        let script =
            format!("set -eu; umask 077; mkdir -p \"$HOME/{parent}\"; cat > \"{remote_path}\"");
        ssh_plumbing(
            &prepared.ssh_target,
            &script,
            prepared.identity.as_deref(),
            Some(&data),
            &relay,
            None,
        )?;
    }
    if let Some(repo) = repo {
        ssh_plumbing(
            &prepared.ssh_target,
            &clone_script(repo, "/app"),
            prepared.identity.as_deref(),
            None,
            &relay,
            None,
        )?;
    }
    ssh_plumbing(
        &prepared.ssh_target,
        "sync",
        prepared.identity.as_deref(),
        None,
        &relay,
        None,
    )?;
    Ok(())
}

fn merge_config(path: &str, local: &[u8], remote: &[u8]) -> Result<Vec<u8>> {
    fn merge(base: &mut serde_json::Value, platform: serde_json::Value) {
        if let (Some(base), Some(platform)) = (base.as_object_mut(), platform.as_object()) {
            for (key, value) in platform {
                merge(
                    base.entry(key).or_insert(serde_json::Value::Null),
                    value.clone(),
                );
            }
        } else {
            *base = platform;
        }
    }
    if path.ends_with(".json") || path.ends_with(".jsonc") || path.ends_with(".toml") {
        let parse = |data: &[u8]| -> Result<serde_json::Value> {
            if data.is_empty() {
                return Ok(serde_json::json!({}));
            }
            Ok(if path.ends_with(".toml") {
                serde_json::to_value(toml::from_str::<toml::Value>(std::str::from_utf8(data)?)?)?
            } else {
                serde_json::from_slice(&strip_json_comments(data))?
            })
        };
        let mut value = parse(local)?;
        merge(&mut value, parse(remote)?);
        return if path.ends_with(".toml") {
            Ok(toml::to_string(&value)?.into_bytes())
        } else {
            Ok(serde_json::to_vec_pretty(&value)?)
        };
    }
    if !remote.is_empty() && remote != local {
        return Ok([remote, b"\n\n", local].concat());
    }
    Ok(local.to_vec())
}

// Blank comment bytes rather than reconstructing characters: UTF-8 strings and
// URLs stay intact. Strip trailing commas only outside strings, after comments.
fn strip_json_comments(input: &[u8]) -> Vec<u8> {
    let mut out = input.to_vec();
    let (mut i, mut quoted, mut escaped) = (0, false, false);
    while i < out.len() {
        let c = out[i];
        if quoted {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                quoted = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            quoted = true;
            i += 1;
            continue;
        }
        if out.get(i..i + 2) == Some(b"//") {
            while i < out.len() && out[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if out.get(i..i + 2) == Some(b"/*") {
            let start = i;
            i += 2;
            while i + 1 < out.len() && &out[i..i + 2] != b"*/" {
                i += 1;
            }
            i = (i + 2).min(out.len());
            out[start..i].fill(b' ');
        } else {
            i += 1;
        }
    }
    quoted = false;
    escaped = false;
    for i in 0..out.len() {
        let c = out[i];
        if quoted {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                quoted = false;
            }
        } else if c == b'"' {
            quoted = true;
        } else if c == b','
            && out[i + 1..]
                .iter()
                .find(|c| !c.is_ascii_whitespace())
                .is_some_and(|c| matches!(c, b'}' | b']'))
        {
            out[i] = b' ';
        }
    }
    out
}

fn clone_script(repo: &str, workspace: &str) -> String {
    let workspace = crate::util::shell::shell_quote(workspace);
    let repo = crate::util::shell::shell_quote(repo);
    format!(
        r#"set -eu
export GIT_TERMINAL_PROMPT=0
[ ! -r "$HOME/.gh-token" ] || export GH_TOKEN="$(cat "$HOME/.gh-token")"
repo={repo}
workspace={workspace}
if [ -d "$workspace/.git" ]; then
    [ "$(git -C "$workspace" remote get-url origin)" = "$repo" ] || {{ echo 'The setup VM already contains a different repository' >&2; exit 1; }}
else
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    timeout 300 git -c credential.helper='!gh auth git-credential' clone -- "$repo" "$tmp/repo"
    mkdir -p "$workspace"
    cp -a "$tmp/repo/." "$workspace/"
fi
sync
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::cloud_agent::tui::app::Target;
    use crate::testkit::MockBackboard;
    use serde_json::json;

    fn request() -> Request {
        Request {
            target: Target {
                project_id: "project".into(),
                project_name: "Demo".into(),
                environment_id: "env".into(),
                environment_name: "production".into(),
            },
            name: "dev".into(),
            repo: None,
            harness: "railway".into(),
        }
    }
    struct Quiet;
    impl Progress for Quiet {
        fn step(&self, _: &str) {}
        fn note(&self, _: &str) {}
        fn finish(&self) {}
    }
    fn stub_vm(server: &MockBackboard) {
        server.stub("AgentBootstraps", json!({"agentBootstraps": []}));
        server.stub("CloudAgentCreate", json!({"cloudAgentCreate": {"id": "setup-vm", "name": "setup-vm", "status": "RUNNING", "projectId": "project", "environmentId": "env", "createdAt": "2026-09-11T00:00:00Z"}}));
        server.stub("CloudAgentDelete", json!({"cloudAgentDelete": true}));
    }
    fn saved(status: &str) -> serde_json::Value {
        json!({"id": "checkpoint-bootstrap", "name": "dev", "environmentId": "env", "status": status, "failureReason": null, "updatedAt": "2026-09-11T00:00:00Z"})
    }

    #[tokio::test]
    async fn bootstrap_setup_saves_ready_checkpoint_sets_default_then_deletes_only_setup_vm() {
        let server = MockBackboard::spawn();
        let dir = tempfile::tempdir().unwrap();
        let mut configs = server.configs(&dir);
        configs
            .set_agent_bootstrap_default("env", "previous", false)
            .await
            .unwrap();
        stub_vm(&server);
        server.stub(
            "AgentBootstrapSave",
            json!({"agentBootstrapSave": saved("SAVING")}),
        );
        server.stub("AgentBootstrap", json!({"agentBootstrap": saved("READY")}));
        let result = create_with(
            &mut configs,
            &reqwest::Client::new(),
            &server.url(),
            &request(),
            &Quiet,
            |agent| async move {
                assert_eq!(agent.id, "setup-vm");
                Ok(())
            },
        )
        .await
        .unwrap();
        assert!(result.is_default);
        configs.reload().unwrap();
        assert_eq!(
            configs.get_agent_bootstrap_default("env"),
            Some("checkpoint-bootstrap")
        );
        assert_eq!(configs.get_code_agent("env"), None);
        let operations: Vec<_> = server
            .requests()
            .iter()
            .map(|r| r["operationName"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            operations,
            [
                "AgentBootstraps",
                "CloudAgentCreate",
                "AgentBootstrapSave",
                "AgentBootstrap",
                "CloudAgentDelete"
            ]
        );
        assert_eq!(
            server.variables_for("CloudAgentDelete")[0]["id"],
            "setup-vm"
        );
        assert!(server.variables_for("CloudAgentCreate")[0]["input"]["agentBootstrapId"].is_null());
    }

    #[tokio::test]
    async fn bootstrap_setup_failures_cleanup_and_preserve_previous_default() {
        for failure in ["configure", "capture"] {
            let server = MockBackboard::spawn();
            let dir = tempfile::tempdir().unwrap();
            let mut configs = server.configs(&dir);
            configs
                .set_agent_bootstrap_default("env", "previous", false)
                .await
                .unwrap();
            stub_vm(&server);
            server.stub(
                "AgentBootstrapSave",
                json!({"agentBootstrapSave": saved("DEGRADED")}),
            );
            let error = create_with(
                &mut configs,
                &reqwest::Client::new(),
                &server.url(),
                &request(),
                &Quiet,
                |_| async move {
                    if failure == "configure" {
                        bail!("clone failed");
                    }
                    Ok(())
                },
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("setup VM was deleted"));
            configs.reload().unwrap();
            assert_eq!(configs.get_agent_bootstrap_default("env"), Some("previous"));
            assert_eq!(server.variables_for("CloudAgentDelete").len(), 1);
            if failure == "configure" {
                assert!(server.variables_for("AgentBootstrapSave").is_empty());
            }
        }
    }

    #[tokio::test]
    async fn bootstrap_setup_cleanup_failure_reports_saved_checkpoint_and_remaining_vm() {
        let server = MockBackboard::spawn();
        let dir = tempfile::tempdir().unwrap();
        let mut configs = server.configs(&dir);
        server.stub("AgentBootstraps", json!({"agentBootstraps": []}));
        server.stub("CloudAgentCreate", json!({"cloudAgentCreate": {"id": "setup-vm", "name": "setup-vm", "status": "RUNNING", "projectId": "project", "environmentId": "env", "createdAt": "2026-09-11T00:00:00Z"}}));
        server.stub(
            "AgentBootstrapSave",
            json!({"agentBootstrapSave": saved("READY")}),
        );
        server.stub_graphql_error("CloudAgentDelete", "delete refused");
        let error = create_with(
            &mut configs,
            &reqwest::Client::new(),
            &server.url(),
            &request(),
            &Quiet,
            |_| async { Ok(()) },
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Bootstrap saved and selected locally")
        );
        assert!(error.to_string().contains("railway ca delete setup-vm"));
        assert_eq!(
            configs.get_agent_bootstrap_default("env"),
            Some("checkpoint-bootstrap")
        );
    }

    #[test]
    fn bootstrap_setup_repo_validation_and_managed_config_merge() {
        assert_eq!(
            repository_url("railwayapp/cli").unwrap(),
            "https://github.com/railwayapp/cli"
        );
        for bad in [
            "--upload-pack=evil",
            "file:///tmp/repo",
            "https://token@github.com/a/b",
            "a/b/c",
            "https://github.com/a/b?token=secret",
            "https://github.com/a/b\nexit",
        ] {
            assert!(repository_url(bad).is_err(), "{bad}");
        }
        let config = merge_config("config.toml", b"model = 'user-model'\n[mcp_servers.custom]\nurl='https://custom'\n[mcp_servers.railway]\nurl='https://wrong'", b"[mcp_servers.railway]\nurl='https://platform'").unwrap();
        let parsed: toml::Value = toml::from_str(std::str::from_utf8(&config).unwrap()).unwrap();
        assert_eq!(parsed["model"].as_str(), Some("user-model"));
        assert_eq!(
            parsed["mcp_servers"]["railway"]["url"].as_str(),
            Some("https://platform")
        );
        assert_eq!(
            parsed["mcp_servers"]["custom"]["url"].as_str(),
            Some("https://custom")
        );
        let jsonc = merge_config(
            "opencode.jsonc",
            "{/* note */\"label\":\"日本語 https://example.com\", // hi\n\"mcp\":{},}".as_bytes(),
            br#"{"mcp":{"railway":{"url":"managed"}}}"#,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&jsonc).unwrap();
        assert_eq!(value["label"], "日本語 https://example.com");
        assert_eq!(value["mcp"]["railway"]["url"], "managed");
    }

    #[cfg(unix)]
    #[test]
    fn bootstrap_setup_clone_is_literal_and_repeatable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("git");
        std::fs::write(
            &git,
            r#"#!/bin/sh
if [ "$1" = '-C' ]; then cat "$2/.git/origin"; exit; fi
[ "$3" = clone ] || exit 2
mkdir -p "$6/.git"
printf '%s\n' "$5" > "$6/.git/origin"
printf 'cloned' > "$6/README"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
        let workspace = dir.path().join("work space");
        let marker = dir.path().join("injected");
        let repo = format!("https://example.com/repo'$(touch {})", marker.display());
        let script = clone_script(&repo, workspace.to_str().unwrap());
        for _ in 0..2 {
            let out = std::process::Command::new("sh")
                .args(["-c", &script])
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("HOME", dir.path())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(!marker.exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join("README")).unwrap(),
            "cloned"
        );
    }
}
