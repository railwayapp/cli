//! OpenCode's local-client flow. CA's terminal sessions still use `prepare`.
use std::{collections::HashMap, fmt, time::Duration};

use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use is_terminal::IsTerminal;

use super::{CliProgress, ConnectInfo, LaunchArgs, Progress, RelayAccess, SessionStyle};
use crate::client::GQLClient;
use crate::commands::cloud_agent::{
    access, desktop,
    opencode::{self, Connection, local},
};
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;

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

pub(super) async fn start(mut args: LaunchArgs, beta: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    access::ensure_enabled(&client, &configs).await?;
    pin_agent(&mut args).await?;
    let directory = args.remote_dir.take().unwrap_or_else(|| "/app".into());
    args.app_mode = true;
    let password = opencode::generate_password();
    args.boot_variables
        .insert("OPENCODE_SERVER_USERNAME".into(), "opencode".into());
    args.boot_variables
        .insert("OPENCODE_SERVER_PASSWORD".into(), password.clone());
    let progress = CliProgress::default();
    let result = super::prepare(&args, &progress, SessionStyle::FullTerminal).await;
    progress.finish();
    let prepared = result?;
    println!(
        "\nStarting {} in the background and opening its authenticated HTTPS endpoint...",
        edition(beta)
    );
    let connection = opencode::start_prepared(&prepared, &directory, &password, beta).await?;
    let desktop = desktop::configure_installed_opencode(
        beta,
        &connection,
        &prepared.agent_id,
        &prepared.agent_name,
    )
    .await;
    clear_setup_output();
    show_connection(&connection, beta, &prepared.agent_name, &desktop)?;
    if interactive()
        && local::confirm(&format!(
            "Launch {} and connect to the Railway Cloud Agent now?",
            edition(beta)
        ))?
    {
        launch(&connection, beta, &prepared.agent_name, &desktop).await?;
    } else if interactive() {
        clear_setup_output();
        show_connection(&connection, beta, &prepared.agent_name, &desktop)?;
    }
    super::ssh_tel::drain_detached(Duration::from_secs(2)).await;
    Ok(())
}

fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Clear setup chatter only after the server is ready. Failed launches retain
/// their diagnostics, and redirected output stays free of terminal controls.
fn clear_setup_output() {
    if interactive() {
        let _ = console::Term::stdout().clear_screen();
    }
}

fn edition(beta: bool) -> &'static str {
    if beta { "OpenCode2 [Beta]" } else { "OpenCode" }
}

fn show_connection(
    connection: &Connection,
    beta: bool,
    name: &str,
    desktop: &Result<bool>,
) -> Result<()> {
    opencode::show_connection(connection, beta, name, matches!(desktop, Ok(true)))?;
    match desktop {
        Ok(true) => {}
        Ok(false) => println!(
            "{} Desktop not detected; desktop configuration skipped.",
            edition(beta)
        ),
        Err(error) => eprintln!(
            "Could not automatically configure {} Desktop: {error:#}\nThe server is still running. Use the connection details above, or rerun this command to retry.",
            edition(beta)
        ),
    }
    Ok(())
}

async fn launch(
    connection: &Connection,
    beta: bool,
    name: &str,
    desktop: &Result<bool>,
) -> Result<()> {
    // The install prompt is deliberately after Enter/selection, and only for
    // the missing edition. Esc/Ctrl+C must never install or stop the server.
    let Some(binary) = local::ensure_client(beta).await? else {
        clear_setup_output();
        return show_connection(connection, beta, name, desktop);
    };
    println!("Launching local {}…", edition(beta));
    let result = local::run_client(&binary, connection, beta);
    println!("\nThe server on {name} is still running.");
    show_connection(connection, beta, name, desktop)?;
    let status = result?;
    if !status.success() {
        bail!("The local {} client exited with {status}.", edition(beta));
    }
    Ok(())
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
            "No running servers for this OpenCode edition. Run railway code with the matching --opencode or --opencode2 flag to set one up."
        ),
        1 if complete => Ok(candidates.pop()),
        _ if !interactive => bail!(
            "Select an OpenCode server explicitly (discovery may be incomplete). Use connect <cloud-agent-name> (or its ID):\n{}",
            candidates
                .iter()
                .map(|c| format!("  {} [id: {}]", c.label, c.agent.id))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        _ => {
            match inquire::Select::new("Connect to an OpenCode server:", candidates)
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
    beta: bool,
    selector: Option<String>,
) -> Result<()> {
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
        let progress = CliProgress::default();
        progress.step("Finding running OpenCode servers");
        let probes: Vec<_> = stream::iter(
            agents
                .into_iter()
                .filter(|a| a.status == ca::Status::Running),
        )
        .map(|agent| {
            let info = relay_info(&agent, &relay);
            async move {
                let result = opencode::inspect(&info, beta).await;
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
                Ok(Some(info)) if !info.directory.is_empty() => candidates.push(Candidate {
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
        let Some(candidate) = choose(candidates, interactive(), complete)? else {
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
        println!("Waking {}…", selected.name);
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
    let connection = opencode::reconnect(&relay_info(&selected, &relay), beta)
        .await
        .with_context(|| format!("Connecting to {} ({})", selected.name, edition(beta)))?;
    let desktop =
        desktop::configure_installed_opencode(beta, &connection, &selected.id, &selected.name)
            .await;
    clear_setup_output();
    show_connection(&connection, beta, &selected.name, &desktop)?;
    if interactive() {
        launch(&connection, beta, &selected.name, &desktop).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
