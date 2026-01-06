use std::{collections::BTreeMap, path::PathBuf, process::Stdio};

use anyhow::{Context, Result};
use colored::{Color, Colorize};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
};

pub const COLORS: &[Color] = &[
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::Blue,
    Color::Red,
];

#[derive(Debug, Clone)]
pub struct LogLine {
    pub service_name: String,
    pub message: String,
    pub is_stderr: bool,
    pub color: Color,
}

struct ManagedProcess {
    service_name: String,
    child: Child,
    color: Color,
    command: String,
    working_dir: PathBuf,
    env_vars: BTreeMap<String, String>,
}

pub struct ProcessManager {
    processes: Vec<ManagedProcess>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Vec::new(),
        }
    }

    pub async fn spawn_service(
        &mut self,
        service_name: String,
        command: &str,
        working_dir: PathBuf,
        env_vars: BTreeMap<String, String>,
        log_tx: mpsc::Sender<LogLine>,
    ) -> Result<()> {
        let color = COLORS[self.processes.len() % COLORS.len()];

        #[cfg(unix)]
        let mut child = Command::new("sh")
            .args(["-c", command])
            .current_dir(&working_dir)
            .envs(env_vars.clone())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .with_context(|| format!("Failed to spawn '{}'", command))?;

        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/C", command])
            .current_dir(&working_dir)
            .envs(env_vars.clone())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("Failed to spawn '{}'", command))?;

        let cmd_log = LogLine {
            service_name: service_name.clone(),
            message: format!("$ {}", command),
            is_stderr: false,
            color,
        };
        let _ = log_tx.send(cmd_log).await;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let name = service_name.clone();
        let tx = log_tx.clone();
        tokio::spawn(async move {
            stream_output(stdout, name, color, false, tx).await;
        });

        let name2 = service_name.clone();
        tokio::spawn(async move {
            stream_output(stderr, name2, color, true, log_tx).await;
        });

        self.processes.push(ManagedProcess {
            service_name,
            child,
            color,
            command: command.to_string(),
            working_dir,
            env_vars,
        });

        Ok(())
    }

    pub async fn restart_service(
        &mut self,
        service_idx: usize,
        log_tx: mpsc::Sender<LogLine>,
    ) -> Result<()> {
        let proc = self.processes.get_mut(service_idx);
        let proc = match proc {
            Some(p) => p,
            None => return Ok(()),
        };

        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            if let Some(pid) = proc.child.id() {
                let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
        }

        #[cfg(windows)]
        {
            let _ = proc.child.kill().await;
        }

        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), proc.child.wait()).await;
        let _ = proc.child.kill().await;

        let service_name = proc.service_name.clone();
        let command = proc.command.clone();
        let working_dir = proc.working_dir.clone();
        let env_vars = proc.env_vars.clone();
        let color = proc.color;

        #[cfg(unix)]
        let mut child = Command::new("sh")
            .args(["-c", &command])
            .current_dir(&working_dir)
            .envs(env_vars.clone())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .with_context(|| format!("Failed to spawn '{}'", command))?;

        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/C", &command])
            .current_dir(&working_dir)
            .envs(env_vars.clone())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("Failed to spawn '{}'", command))?;

        let cmd_log = LogLine {
            service_name: service_name.clone(),
            message: format!("$ {}", command),
            is_stderr: false,
            color,
        };
        let _ = log_tx.send(cmd_log).await;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let name = service_name.clone();
        let tx = log_tx.clone();
        tokio::spawn(async move {
            stream_output(stdout, name, color, false, tx).await;
        });

        let name2 = service_name.clone();
        tokio::spawn(async move {
            stream_output(stderr, name2, color, true, log_tx).await;
        });

        proc.child = child;

        Ok(())
    }

    pub async fn restart_all(&mut self, log_tx: mpsc::Sender<LogLine>) -> Result<()> {
        for i in 0..self.processes.len() {
            self.restart_service(i, log_tx.clone()).await?;
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;

            for proc in &self.processes {
                if let Some(pid) = proc.child.id() {
                    let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM);
                }
            }
        }

        #[cfg(windows)]
        {
            for proc in &mut self.processes {
                let _ = proc.child.kill().await;
            }
        }

        for proc in &mut self.processes {
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(5), proc.child.wait()).await;
        }

        for proc in &mut self.processes {
            let _ = proc.child.kill().await;
        }

        self.processes.clear();
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

async fn stream_output<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    service_name: String,
    color: Color,
    is_stderr: bool,
    tx: mpsc::Sender<LogLine>,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let log = LogLine {
            service_name: service_name.clone(),
            message: line,
            is_stderr,
            color,
        };
        if tx.send(log).await.is_err() {
            break;
        }
    }
}

pub fn print_log_line(log: &LogLine) {
    let prefix = format!("[{}]", log.service_name).color(log.color);
    if log.is_stderr {
        eprintln!("{} {}", prefix, log.message);
    } else {
        println!("{} {}", prefix, log.message);
    }
}
