use std::collections::BTreeMap;

use crate::consts::SERVICE_NOT_FOUND;

use super::*;

/// Open a subshell with Railway variables available
#[derive(Parser)]
pub struct Args {
    /// Service to pull variables from (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;
    let mut all_variables = BTreeMap::<String, String>::new();
    all_variables.insert("IN_RAILWAY_SHELL".to_owned(), "true".to_owned());

    if let Some(service) = args.service {
        let service_id = body
            .project
            .services
            .edges
            .iter()
            .find(|s| s.node.name == service || s.node.id == service)
            .context(SERVICE_NOT_FOUND)?;

        let vars = queries::variables_for_service_deployment::Variables {
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: service_id.node.id.clone(),
        };

        let res = post_graphql::<queries::VariablesForServiceDeployment, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?;

        let mut body = res.data.context("Failed to retrieve response body")?;

        all_variables.append(&mut body.variables_for_service_deployment);
    } else if let Some(service) = linked_project.service {
        let vars = queries::variables_for_service_deployment::Variables {
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: service.clone(),
        };

        let res = post_graphql::<queries::VariablesForServiceDeployment, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?;

        let mut body = res.data.context("Failed to retrieve response body")?;

        all_variables.append(&mut body.variables_for_service_deployment);
    } else {
        eprintln!("No service linked, skipping service variables");
    }

    enum WindowsShell {
        Cmd,
        Powershell,
        Powershell7,
    }

    async fn windows_shell_detection() -> Option<WindowsShell> {
        let pid = std::process::id().to_string();

        // https://stackoverflow.com/questions/7486717/finding-parent-process-id-on-windows
        // runs the command "wmic process get processid,parentprocessid,executablepath|find "process id goes here"
        // this is to determine the parent process of the current process, which should give the shell.
        // Only matches cmd, powershell or pwsh for safety
        let wmic = tokio::process::Command::new("wmic")
            .args(&["process", "get", "executablepath,processid,parentprocessid"])
            .output()
            .await
            .context("Failed to run wmic command")
            .unwrap();
        let wmic = String::from_utf8(wmic.stdout)
            .context("Failed to convert wmic output to utf8")
            .unwrap();

        let current_process = wmic
            .lines()
            .find(|line| line.contains(&pid))
            .context("Failed to find current process in wmic output")
            .unwrap()
            .to_string();

        dbg!(&current_process);

        let pids = current_process.split_whitespace().collect::<Vec<&str>>();
        let pids = pids
            .iter()
            .filter(|pid| pid.parse::<u32>().is_ok())
            .collect::<Vec<&&str>>();

        let parent_pid = pids[0].to_string();

        let parent_output_line = wmic
            .lines()
            .find(|line| line.contains(&parent_pid))
            .context("Failed to find parent process in wmic output")
            .unwrap()
            .to_string();

        let parent_executable = parent_output_line
            .split("exe")
            .next()
            .context("Failed to find parent executable")
            .unwrap()
            .to_string()
            + "exe";

        if parent_executable.contains("pwsh") {
            Some(WindowsShell::Powershell7)
        } else if parent_executable.contains("powershell") {
            Some(WindowsShell::Powershell)
        } else {
            Some(WindowsShell::Cmd)
        }
    }

    let shell = std::env::var("SHELL").unwrap_or(match std::env::consts::OS {
        "windows" => match windows_shell_detection().await {
            Some(WindowsShell::Powershell) => "powershell".to_string(),
            Some(WindowsShell::Cmd) => "cmd".to_string(),
            Some(WindowsShell::Powershell7) => "pwsh".to_string(),
            None => "cmd".to_string(),
        },
        _ => "sh".to_string(),
    });

    let shell_options = match shell.as_str() {
        "powershell" => vec!["/nologo"],
        "pwsh" => vec!["/nologo"],
        "cmd" => vec!["/k"],
        _ => vec![],
    };

    println!("Entering subshell with Railway variables available. Type 'exit' to exit.\n");

    tokio::process::Command::new(shell)
        .args(shell_options)
        .envs(all_variables)
        .spawn()
        .context("Failed to spawn command")?
        .wait()
        .await
        .context("Failed to wait for command")?;

    println!("Exited subshell, Railway variables no longer available.");
    Ok(())
}
