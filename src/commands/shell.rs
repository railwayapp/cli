use anyhow::bail;
use std::collections::BTreeMap;

use crate::{
    controllers::project::{ensure_project_and_environment_exist, get_project},
    controllers::variables::get_service_variables,
    errors::RailwayError,
};

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

/// Open a local subshell with Railway variables available
#[derive(Parser)]
pub struct Args {
    /// Service to pull variables from (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Open shell without banner
    #[clap(long)]
    silent: bool,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let mut all_variables = BTreeMap::<String, String>::new();
    all_variables.insert("IN_RAILWAY_SHELL".to_owned(), "true".to_owned());

    if let Some(service) = args.service {
        let service_id = project
            .services
            .edges
            .iter()
            .find(|s| s.node.name == service || s.node.id == service)
            .ok_or_else(|| RailwayError::ServiceNotFound(service))?;

        let mut variables = get_service_variables(
            &client,
            &configs,
            linked_project.project.clone(),
            linked_project.environment.clone(),
            service_id.node.id.clone(),
        )
        .await?;

        all_variables.append(&mut variables);
    } else if let Some(service) = linked_project.service {
        let mut variables = get_service_variables(
            &client,
            &configs,
            linked_project.project.clone(),
            linked_project.environment.clone(),
            service.clone(),
        )
        .await?;

        all_variables.append(&mut variables);
    } else {
        bail!("No service linked. Please link one with `railway service`");
    }

    let shell = std::env::var("SHELL").unwrap_or(match std::env::consts::OS {
        "windows" => match windows_shell_detection().await {
            Some(shell) => shell.to_string(),
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

    if !args.silent {
        println!("Entering subshell with Railway variables available. Type 'exit' to exit.\n");
    }

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

    if !args.silent {
        println!("Exited subshell, Railway variables no longer available.");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
unsafe fn wrapper_fix_recursive(
    process_id: DWORD,
    recursion: Option<u32>,
) -> Result<(u32, String)> {
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

    let triggers = vec!["node.exe", "railway.exe"];

    if triggers.contains(&ppname.as_str()) {
        wrapper_fix_recursive(ppid, recursion.checked_add(1))
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

impl core::fmt::Display for WindowsShell {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match *self {
            WindowsShell::Powershell => write!(f, "powershell"),
            WindowsShell::Cmd => write!(f, "cmd"),
            WindowsShell::Powershell7 => write!(f, "pwsh"),
            WindowsShell::NuShell => write!(f, "nu"),
            WindowsShell::ElvSh => write!(f, "elvish"),
        }
    }
}

impl std::str::FromStr for WindowsShell {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cmd" => Ok(WindowsShell::Cmd),
            "powershell" => Ok(WindowsShell::Powershell),
            "pwsh" => Ok(WindowsShell::Powershell7),
            "nu" => Ok(WindowsShell::NuShell),
            "elvish" => Ok(WindowsShell::ElvSh),
            _ => Ok(WindowsShell::Cmd),
        }
    }
}

/// https://gist.github.com/mattn/253013/d47b90159cf8ffa4d92448614b748aa1d235ebe4
///
/// defaults to cmd if no parent process is found
///
// How to add new shells:
//
// 1. edit the WindowsShell enum and add the new shell
// 2. edit the impl Display for WindowsShell and add the new shell's stringified name (Powershell7 => "pwsh")
// 3. edit the impl FromStr for WindowsShell and add the new shell's stringified name (pwsh => Powershell7)
#[cfg(target_os = "windows")]
async fn windows_shell_detection() -> Option<WindowsShell> {
    let (ppid, mut ppname) = unsafe {
        get_parent_process_info(None)
            .context("Failed to get parent process info")
            .unwrap_or_else(|_| (0, "".to_string()))
    };

    let triggers = vec!["node.exe", "railway.exe"];

    if triggers.contains(&ppname.as_str()) {
        (_, ppname) = unsafe {
            wrapper_fix_recursive(ppid, None)
                .context("Failed to get parent process info")
                // acceptable return because if it fails it will default to cmd
                .unwrap_or_else(|_| (0, "".to_string()))
        }
    }

    let ppname = ppname.split('.').next().unwrap_or("cmd");

    ppname.parse::<WindowsShell>().ok()
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

    parent_process_name.map(|ppname| (ppid, ppname))
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
