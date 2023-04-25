use std::collections::BTreeMap;

use crate::consts::SERVICE_NOT_FOUND;

use super::*;

/// winapi is only used on windows
#[cfg(target_os = "windows")]
extern crate winapi;
#[cfg(target_os = "windows")]
use winapi::shared::minwindef::DWORD;
#[cfg(target_os = "windows")]
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
#[cfg(target_os = "windows")]
use winapi::um::tlhelp32::{
    CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
};

/// memory management helpers are also only used on windows
#[cfg(target_os = "windows")]
use std::ffi::CStr;
#[cfg(target_os = "windows")]
use std::mem::zeroed;

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
        eprintln!("No service linked, not entering shell.");
        return Ok(());
    }

    let shell = std::env::var("SHELL").unwrap_or(match std::env::consts::OS {
        "windows" => match windows_shell_detection().await {
            Some(WindowsShell::Powershell) => "powershell".to_string(),
            Some(WindowsShell::Cmd) => "cmd".to_string(),
            Some(WindowsShell::Powershell7) => "pwsh".to_string(),
            Some(WindowsShell::NuShell) => "nu".to_string(),
            Some(WindowsShell::ElvSh) => "elvish".to_string(),
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

    // a bit janky :/
    ctrlc::set_handler(move || {
        // do nothing, we just want to ignore CTRL+C
        // this is for `rails c` and similar REPLs
    })?;

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

#[cfg(target_os = "windows")]
unsafe fn node_fix_recursive(process_id: DWORD, recursion: Option<u32>) -> Result<(u32, String)> {
    // recursive because for some reason it occasionally is more than one level deep
    let recursion = recursion.unwrap_or(0);
    if recursion > 10 {
        // no error, just return nothing and it will default to cmd.
        return Ok((0, "".to_string()));
    }

    let (ppid, ppname) = unsafe {
        get_parent_process_info(Some(process_id))
            .context("Failed to get parent process info")
            .unwrap_or_else(|_| (0, "".to_string()))
    };

    if ppname == "node.exe" {
        node_fix_recursive(ppid, recursion.checked_add(1))
    } else {
        Ok((ppid, ppname))
    }
}

// disable dead code warning for windows_shell_detection
#[allow(dead_code)]
enum WindowsShell {
    Cmd,
    Powershell,
    Powershell7,
    NuShell,
    ElvSh,
}

/// https://gist.github.com/mattn/253013/d47b90159cf8ffa4d92448614b748aa1d235ebe4
///
/// defaults to cmd if no parent process is found
#[cfg(target_os = "windows")]
async fn windows_shell_detection() -> Option<WindowsShell> {
    let (ppid, mut ppname) = unsafe {
        get_parent_process_info(None)
            .context("Failed to get parent process info")
            .unwrap_or_else(|_| (0, "".to_string()))
    };

    if ppname == "node.exe" {
        (_, ppname) = unsafe {
            node_fix_recursive(ppid, None)
                .context("Failed to get parent process info")
                // acceptable return because if it fails it will default to cmd
                .unwrap_or_else(|_| (0, "".to_string()))
        }
    }

    let ppname = ppname.split(".").next().unwrap_or("cmd");

    dbg!(ppname);

    match ppname {
        "cmd" => Some(WindowsShell::Cmd),
        "powershell" => Some(WindowsShell::Powershell),
        "pwsh" => Some(WindowsShell::Powershell7),
        "nu" => Some(WindowsShell::NuShell),
        "elvish" => Some(WindowsShell::ElvSh),
        _ => Some(WindowsShell::Cmd),
    }
}

#[cfg(not(target_os = "windows"))]
async fn windows_shell_detection() -> Option<WindowsShell> {
    None
}

/// get the parent process info, translated from
// https://gist.github.com/mattn/253013/d47b90159cf8ffa4d92448614b748aa1d235ebe4
#[cfg(target_os = "windows")]
unsafe fn get_parent_process_info(pid: Option<DWORD>) -> Option<(DWORD, String)> {
    let pid = pid.unwrap_or(std::process::id());

    let mut pe32: PROCESSENTRY32 = unsafe { zeroed() };
    let h_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    let mut ppid = 0;

    if h_snapshot == INVALID_HANDLE_VALUE {
        return None;
    }

    pe32.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;

    if unsafe { Process32First(h_snapshot, &mut pe32) } != 0 {
        loop {
            if pe32.th32ProcessID == pid {
                ppid = pe32.th32ParentProcessID;
                break;
            }
            if unsafe { Process32Next(h_snapshot, &mut pe32) } == 0 {
                break;
            }
        }
    }

    let mut parent_process_name = None;
    if ppid != 0 {
        parent_process_name = get_process_name(ppid);
    }

    unsafe { CloseHandle(h_snapshot) };

    if let Some(ppname) = parent_process_name {
        Some((ppid, ppname))
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
unsafe fn get_process_name(pid: DWORD) -> Option<String> {
    let mut pe32: PROCESSENTRY32 = unsafe { zeroed() };
    let h_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };

    if h_snapshot == INVALID_HANDLE_VALUE {
        return None;
    }

    pe32.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;

    if unsafe { Process32First(h_snapshot, &mut pe32) } != 0 {
        loop {
            if pe32.th32ProcessID == pid {
                let process_name_cstr = unsafe { CStr::from_ptr(pe32.szExeFile.as_ptr()) };
                let process_name = process_name_cstr.to_string_lossy().into_owned();
                unsafe { CloseHandle(h_snapshot) };
                return Some(process_name);
            }
            if unsafe { Process32Next(h_snapshot, &mut pe32) } == 0 {
                break;
            }
        }
    }

    unsafe { CloseHandle(h_snapshot) };
    None
}
