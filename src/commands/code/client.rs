//! Shared local-client flow for Codex and OpenCode. CA terminal sessions use `prepare`.
use std::{collections::HashMap, fmt, time::Duration};

use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use is_terminal::IsTerminal;

use super::saved_config::SavedConfig;
use super::{CliProgress, ConnectInfo, LaunchArgs, Progress, RelayAccess, SessionStyle};
use crate::client::GQLClient;
use crate::commands::cloud_agent::client_sessions::Connection;
use crate::commands::cloud_agent::{
    access, codex, desktop,
    opencode::{self, local},
};
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Harness {
    Codex,
    OpenCode,
    OpenCode2,
}

impl Harness {
    fn edition(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::OpenCode2 => "OpenCode2 [Beta]",
        }
    }

    async fn inspect(self, info: &ConnectInfo) -> Result<bool> {
        match self {
            Self::Codex => codex::inspect(info).await,
            _ => Ok(opencode::inspect(info, self == Self::OpenCode2)
                .await?
                .is_some_and(|info| !info.directory.is_empty())),
        }
    }

    async fn reconnect(self, info: &ConnectInfo) -> Result<Connection> {
        match self {
            Self::Codex => Ok(Connection::Codex(codex::reconnect(info).await?)),
            _ => Ok(Connection::OpenCode(
                opencode::reconnect(info, self == Self::OpenCode2).await?,
                self == Self::OpenCode2,
            )),
        }
    }
}

impl Connection {
    async fn configure_snapshot(
        &self,
        saved: SavedConfig,
        identity: Option<&std::path::Path>,
        options: &desktop::CodexOptions,
        desktop_only: bool,
    ) -> SavedConfig {
        match self {
            Self::Codex(connection) => {
                let desktop = desktop::configure_codex(
                    &saved.agent_name,
                    &saved.environment_id,
                    identity,
                    &connection.directory,
                    options,
                )
                .await;
                saved.with_codex(connection, desktop_only, &desktop)
            }
            Self::OpenCode(connection, beta) => {
                let desktop = desktop::configure_installed_opencode(
                    *beta,
                    connection,
                    &saved.agent_id,
                    &saved.agent_name,
                )
                .await;
                saved.with_opencode(connection, *beta, &desktop)
            }
        }
    }

    fn show(&self, saved: &SavedConfig, persisted: &Result<()>) -> Result<()> {
        saved.show()?;
        if let Err(error) = persisted {
            eprintln!("Could not save connection details for railway code get-config: {error:#}");
        }
        Ok(())
    }

    fn json(&self) -> serde_json::Value {
        match self {
            Self::Codex(c) => serde_json::json!({
                "url": c.url, "token": c.token, "directory": c.directory,
                "version": c.version, "reused": c.reused,
            }),
            Self::OpenCode(c, _) => serde_json::json!({
                "url": c.url, "username": c.username, "password": c.password,
                "directory": c.directory, "reused": c.reused,
            }),
        }
    }
}

pub(super) async fn pin_agent(args: &mut LaunchArgs) -> Result<()> {
    if let Some(selector) = args.remote_agent.take() {
        let configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        let (agent, _) = ca::resolve(&configs, &client, Some(&selector), None).await?;
        args.agent_id = Some(agent.id);
        args.project = Some(agent.project_id);
        args.environment = Some(agent.environment_id);
    }
    Ok(())
}

pub(super) enum LaunchMode {
    LocalClient,
    DesktopOnly(desktop::CodexOptions),
}

/// The CA launch pipeline for server-backed clients. Provisioning and runtime
/// setup stay behind the frame; only the finished local PTY enters the pane.
pub(crate) async fn prepare_pane(
    mut args: LaunchArgs,
    harness: &str,
    progress: &dyn Progress,
) -> Result<crate::commands::cloud_agent::tui::ClientPane> {
    let beta = harness == "opencode2";
    let directory = args.remote_dir.take();
    let password = opencode::generate_password();
    args.app_mode = true;
    if harness != "codex" {
        args.boot_variables
            .insert("OPENCODE_SERVER_USERNAME".into(), "opencode".into());
        args.boot_variables
            .insert("OPENCODE_SERVER_PASSWORD".into(), password.clone());
    }
    let prepared = super::prepare(&args, progress, SessionStyle::Pane).await?;
    let directory = directory
        .or_else(|| {
            super::saved_config::client_connection(&prepared.agent_id, &prepared.environment_id)
                .filter(|c| c.harness() == harness)
                .map(|c| c.directory().to_string())
        })
        .unwrap_or_else(|| "/app".into());
    let result: Result<_> = async {
        progress.step(&format!("Starting {harness} server"));
        let connection = if harness == "codex" {
            Connection::Codex(codex::start_prepared(&prepared, &directory, &password).await?)
        } else {
            Connection::OpenCode(
                opencode::start_prepared(&prepared, &directory, &password, beta).await?,
                beta,
            )
        };
        let saved = connection
            .configure_snapshot(
                SavedConfig::from_prepared(&prepared)?,
                prepared.identity.as_deref(),
                &desktop::CodexOptions::default(),
                false,
            )
            .await;
        saved.save()?;
        progress.step(&format!("Preparing local {harness} client"));
        let binary = match &connection {
            Connection::Codex(c) => codex::local::ensure_client(&c.version).await?,
            Connection::OpenCode(_, beta) => local::ensure_client_quiet(*beta).await?,
        };
        let thread = connection
            .new_thread(args.initial_prompt.as_deref())
            .await?;
        let prompt = connection
            .initial_prompt(thread.as_ref(), args.initial_prompt)
            .await?;
        Ok(crate::commands::cloud_agent::tui::ClientPane {
            agent_id: prepared.agent_id.clone(),
            agent_name: prepared.agent_name.clone(),
            environment_id: prepared.environment_id.clone(),
            binary,
            connection,
            thread,
            prompt,
        })
    }
    .await;
    result.with_context(|| {
        format!(
            "Opening {harness} on {} ({})",
            prepared.agent_name, prepared.agent_id
        )
    })
}

pub(super) async fn start(mut args: LaunchArgs, harness: Harness, mode: LaunchMode) -> Result<()> {
    let desktop_only = matches!(&mode, LaunchMode::DesktopOnly(_));
    let desktop_options = match mode {
        LaunchMode::LocalClient => desktop::CodexOptions::default(),
        LaunchMode::DesktopOnly(options) => options,
    };
    if harness == Harness::Codex {
        desktop_options.validate()?;
        desktop::preflight_codex_desktop()?;
    }
    let json = args.connection_json;
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    access::ensure_enabled(&client, &configs).await?;
    pin_agent(&mut args).await?;
    let directory = args.remote_dir.take().unwrap_or_else(|| "/app".into());
    args.app_mode = true;
    let password = opencode::generate_password();
    if harness != Harness::Codex {
        args.boot_variables
            .insert("OPENCODE_SERVER_USERNAME".into(), "opencode".into());
        args.boot_variables
            .insert("OPENCODE_SERVER_PASSWORD".into(), password.clone());
    }
    let progress = ConnectionProgress::new(json);
    let result = super::prepare(&args, &progress, SessionStyle::FullTerminal).await;
    progress.finish();
    let prepared = result?;
    if json {
        eprintln!(
            "Starting {} and checking its public endpoint",
            harness.edition()
        );
    } else {
        println!(
            "\nStarting {} in the background and opening its authenticated endpoint...",
            harness.edition()
        );
        if harness == Harness::OpenCode2 {
            println!("The first start downloads the latest Beta and can take several minutes.");
        }
    }
    let connection = match harness {
        Harness::Codex => {
            Connection::Codex(codex::start_prepared(&prepared, &directory, &password).await?)
        }
        _ => Connection::OpenCode(
            opencode::start_prepared(
                &prepared,
                &directory,
                &password,
                harness == Harness::OpenCode2,
            )
            .await?,
            harness == Harness::OpenCode2,
        ),
    };
    let saved = connection
        .configure_snapshot(
            SavedConfig::from_prepared(&prepared)?,
            prepared.identity.as_deref(),
            &desktop_options,
            desktop_only,
        )
        .await;
    let persisted = saved.save();
    if desktop_only && saved.require_desktop().is_ok() {
        crate::commands::cloud_agent::telemetry::track_desktop_configured("codex").await;
    }
    if json {
        if let Err(error) = &persisted {
            eprintln!("Could not save connection details for railway code get-config: {error:#}");
        }
        print_connection_json(
            &connection,
            &prepared.agent_id,
            &prepared.agent_name,
            &prepared.environment_id,
        )?;
        super::ssh_tel::drain_detached(Duration::from_secs(2)).await;
        return if desktop_only {
            saved.require_desktop()
        } else {
            Ok(())
        };
    }
    clear_setup_output();
    connection.show(&saved, &persisted)?;
    if desktop_only {
        super::ssh_tel::drain_detached(Duration::from_secs(2)).await;
        return saved.require_desktop();
    }
    if interactive() {
        launch(
            &connection,
            harness,
            &saved,
            &persisted,
            args.initial_prompt.clone(),
        )
        .await?;
    }
    super::ssh_tel::drain_detached(Duration::from_secs(2)).await;
    Ok(())
}

fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Preserve diagnostics on failures and keep redirected output free of escapes.
fn clear_setup_output() {
    if interactive() {
        let _ = console::Term::stdout().clear_screen();
    }
}

struct ConnectionProgress {
    json: bool,
    cli: CliProgress,
}

impl ConnectionProgress {
    fn new(json: bool) -> Self {
        Self {
            json,
            cli: CliProgress::default(),
        }
    }
}

impl Progress for ConnectionProgress {
    fn step(&self, text: &str) {
        if self.json {
            eprintln!("{text}");
        } else {
            self.cli.step(text);
        }
    }

    fn note(&self, text: &str) {
        if self.json {
            eprintln!("{text}");
        } else {
            self.cli.note(text);
        }
    }

    fn finish(&self) {
        self.cli.finish();
    }
}

fn connection_json(
    connection: &Connection,
    id: &str,
    name: &str,
    environment_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "agent": { "id": id, "name": name, "environmentId": environment_id },
        "connection": connection.json(),
    })
}

fn print_connection_json(
    connection: &Connection,
    id: &str,
    name: &str,
    environment_id: &str,
) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(&connection_json(connection, id, name, environment_id))?
    );
    Ok(())
}

async fn launch(
    connection: &Connection,
    harness: Harness,
    saved: &SavedConfig,
    persisted: &Result<()>,
    prompt: Option<String>,
) -> Result<()> {
    // Codex is version-matched automatically; OpenCode still offers installation.
    let binary = match connection {
        Connection::Codex(c) => Some(codex::local::ensure_client(&c.version).await?),
        Connection::OpenCode(_, beta) => local::ensure_client(*beta).await?,
    };
    let Some(binary) = binary else {
        clear_setup_output();
        return connection.show(saved, persisted);
    };
    println!("Launching local {}…", harness.edition());
    let thread = connection.new_thread(prompt.as_deref()).await?;
    let prompt = connection.initial_prompt(thread.as_ref(), prompt).await?;
    let result = crate::commands::cloud_agent::launch_client_in_pane(
        crate::commands::cloud_agent::tui::ClientPane {
            agent_id: saved.agent_id.clone(),
            agent_name: saved.agent_name.clone(),
            environment_id: saved.environment_id.clone(),
            binary,
            connection: connection.clone(),
            thread,
            prompt,
        },
    )
    .await;
    if result.is_ok() {
        clear_setup_output();
    }
    connection.show(saved, persisted)?;
    result
}

struct Candidate {
    agent: ca::Agent,
    label: String,
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.label.fmt(f)
    }
}

fn relay_info(agent: &ca::Agent, access: &RelayAccess) -> ConnectInfo {
    ConnectInfo {
        ssh_target: format!("agent:{}:{}", agent.environment_id, agent.id),
        identity: access.identity.clone(),
        relay_opts: access.relay_opts.clone(),
    }
}

fn label(agent: &ca::Agent, names: &HashMap<String, (String, String)>) -> String {
    let (workspace, project) = names
        .get(&agent.project_id)
        .cloned()
        .unwrap_or_else(|| ("Unknown workspace".into(), agent.project_id.clone()));
    format!("{workspace}/{project}/{}", agent.name)
}

fn choose(
    mut candidates: Vec<Candidate>,
    interactive: bool,
    complete: bool,
) -> Result<Option<Candidate>> {
    candidates.sort_by(|a, b| {
        a.label
            .cmp(&b.label)
            .then_with(|| a.agent.id.cmp(&b.agent.id))
    });
    let mut counts = HashMap::new();
    for candidate in &candidates {
        *counts.entry(candidate.label.clone()).or_insert(0) += 1;
    }
    for candidate in &mut candidates {
        if counts[&candidate.label] > 1 {
            candidate.label = format!("{} ({})", candidate.label, candidate.agent.id);
        }
    }
    match candidates.len() {
        0 => bail!(
            "No running servers for this client. Run railway code with the matching --codex, --opencode, or --opencode2 flag to set one up."
        ),
        1 if complete => Ok(candidates.pop()),
        _ if !interactive => bail!(
            "Select a server explicitly (discovery may be incomplete). Use connect <cloud-agent-name> (or its ID):\n{}",
            candidates
                .iter()
                .map(|c| format!("  {} [id: {}]", c.label, c.agent.id))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        _ => {
            match inquire::Select::new("Connect to a server:", candidates)
                .with_render_config(Configs::get_render_config())
                .prompt()
            {
                Ok(candidate) => Ok(Some(candidate)),
                Err(
                    inquire::InquireError::OperationCanceled
                    | inquire::InquireError::OperationInterrupted,
                ) => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
    }
}

pub(super) async fn connect(
    mut args: LaunchArgs,
    harness: Harness,
    selector: Option<String>,
) -> Result<()> {
    if harness == Harness::Codex {
        desktop::preflight_codex_desktop()?;
    }
    let json = args.connection_json;
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    access::ensure_enabled(&client, &configs).await?;
    let backboard = configs.get_backboard();
    let scoped = if args.project.is_some() || args.environment.is_some() {
        Some(
            super::resolve_project_and_env(
                &mut configs,
                &client,
                args.project.take(),
                args.environment.take(),
            )
            .await?
            .1,
        )
    } else {
        None
    };
    let relay = super::relay_access().await?;
    let selected = if let Some(selector) = selector {
        let (agent, _) = ca::resolve(&configs, &client, Some(&selector), scoped.as_deref()).await?;
        agent
    } else {
        let agents = if let Some(environment) = &scoped {
            ca::list_in_environment(&client, &backboard, environment, true).await?
        } else {
            ca::list_mine(&client, &backboard).await?
        };
        let workspaces = crate::workspace::workspaces_with_client(&client, &configs).await?;
        let mut names = HashMap::new();
        for workspace in workspaces {
            for project in workspace.projects() {
                names.insert(
                    project.id().to_string(),
                    (workspace.name().to_string(), project.name().to_string()),
                );
            }
        }
        let progress = ConnectionProgress::new(json);
        progress.step(&format!("Finding running {} servers", harness.edition()));
        let probes: Vec<_> = stream::iter(
            agents
                .into_iter()
                .filter(|a| a.status == ca::Status::Running),
        )
        .map(|agent| {
            let info = relay_info(&agent, &relay);
            async move {
                let result = harness.inspect(&info).await;
                (agent, result)
            }
        })
        .buffer_unordered(6)
        .collect()
        .await;
        progress.finish();
        let mut candidates = Vec::new();
        let mut complete = true;
        for (agent, result) in probes {
            match result {
                Ok(true) => candidates.push(Candidate {
                    label: label(&agent, &names),
                    agent,
                }),
                Ok(_) => {}
                Err(error) => {
                    complete = false;
                    eprintln!("Could not check {}: {error}", label(&agent, &names));
                }
            }
        }
        let Some(candidate) = choose(candidates, !json && interactive(), complete)? else {
            println!(
                "Connection canceled. Your servers are still running; rerun connect to choose one."
            );
            return Ok(());
        };
        candidate.agent
    };
    if !selected.status.is_live() {
        bail!(
            "Agent {} is {} and cannot be connected to.",
            selected.name,
            selected.status.label()
        );
    }
    if selected.status == ca::Status::Sleeping {
        if json {
            eprintln!("Waking {}…", selected.name);
        } else {
            println!("Waking {}…", selected.name);
        }
        ca::wake(&client, &backboard, &selected.id).await?;
    }
    if selected.status != ca::Status::Running {
        super::wait_until_connectable(
            &client,
            &backboard,
            &selected.environment_id,
            &selected.id,
            &relay,
            Duration::from_millis(1500),
        )
        .await?;
    }
    let connection = harness
        .reconnect(&relay_info(&selected, &relay))
        .await
        .with_context(|| format!("Connecting to {} ({})", selected.name, harness.edition()))?;
    let slug = match harness {
        Harness::Codex => "codex",
        Harness::OpenCode => "opencode",
        Harness::OpenCode2 => "opencode2",
    };
    let saved = connection
        .configure_snapshot(
            SavedConfig::new(
                &selected.id,
                &selected.name,
                &selected.environment_id,
                slug,
                relay.identity.as_deref(),
            )?,
            relay.identity.as_deref(),
            &desktop::CodexOptions::default(),
            false,
        )
        .await;
    let persisted = saved.save();
    if json {
        if let Err(error) = &persisted {
            eprintln!("Could not save connection details for railway code get-config: {error:#}");
        }
        return print_connection_json(
            &connection,
            &selected.id,
            &selected.name,
            &selected.environment_id,
        );
    }
    clear_setup_output();
    connection.show(&saved, &persisted)?;
    if interactive() {
        launch(&connection, harness, &saved, &persisted, None).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_json_is_versioned_and_keeps_credentials_out_of_the_url() {
        let connection = opencode::Connection {
            url: "https://agent.example.com".into(),
            username: "opencode".into(),
            password: "secret with \"quotes\"\nand a newline".into(),
            directory: "/app/a project".into(),
            reused: true,
        };
        let encoded = serde_json::to_string(&connection_json(
            &Connection::OpenCode(
                opencode::Connection {
                    url: connection.url.clone(),
                    username: connection.username.clone(),
                    password: connection.password.clone(),
                    directory: connection.directory.clone(),
                    reused: true,
                },
                true,
            ),
            "agent-id",
            "box",
            "env-id",
        ))
        .unwrap();
        assert_eq!(encoded.lines().count(), 1);
        let output: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(output["schemaVersion"], 1);
        assert_eq!(output["agent"]["id"], "agent-id");
        assert_eq!(output["agent"]["environmentId"], "env-id");
        assert_eq!(output["connection"]["password"], connection.password);
        assert_eq!(output["connection"]["url"], connection.url);
        assert_eq!(output["connection"]["directory"], connection.directory);
        assert_eq!(output["connection"]["reused"], true);
    }

    fn candidate(id: &str, name: &str) -> Candidate {
        let agent = ca::Agent {
            id: id.into(),
            name: name.into(),
            status: ca::Status::Running,
            project_id: "project".into(),
            environment_id: "env".into(),
            created_at: chrono::Utc::now(),
        };
        let names = HashMap::from([("project".into(), ("Workspace".into(), "Project".into()))]);
        Candidate {
            label: label(&agent, &names),
            agent,
        }
    }

    #[test]
    fn selection_uses_only_server_candidates_and_never_guesses_between_them() {
        assert!(choose(vec![], false, true).is_err());
        assert!(choose(vec![candidate("a", "box")], false, false).is_err());
        let sole = choose(vec![candidate("a", "box")], false, true)
            .unwrap()
            .unwrap();
        assert_eq!(sole.agent.id, "a");
        assert_eq!(sole.to_string(), "Workspace/Project/box");
        let error = choose(
            vec![candidate("a", "box"), candidate("b", "box")],
            false,
            true,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("Workspace/Project/box (a)"));
        assert!(error.contains("Workspace/Project/box (b)"));
    }
}
