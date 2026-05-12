mod app;
mod ui;

use std::{
    env, fs,
    io::{self, stdout},
    panic,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
};

pub use app::{BrowserAction, PendingTransfer, RemoteEntry, VolumeBrowserApp};

use anyhow::{Context, Result, bail};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

const SSH_HOST: &str = "ssh.railway.com";

pub enum VolumeBrowserOutput {
    Closed,
}

pub struct VolumeBrowserParams {
    pub service_instance_id: String,
    pub service_name: String,
    pub volume_name: String,
    pub mount_path: PathBuf,
    pub local_dir: PathBuf,
    pub identity_file: Option<PathBuf>,
}

pub fn run(params: VolumeBrowserParams) -> Result<VolumeBrowserOutput> {
    fs::create_dir_all(&params.local_dir).with_context(|| {
        format!(
            "Failed to create local transfer directory {}",
            params.local_dir.display()
        )
    })?;

    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let remote = NativeRemote::connect(params.service_instance_id, params.identity_file)?;
    let mut app = VolumeBrowserApp::new(
        params.service_name,
        params.volume_name,
        params.mount_path,
        params.local_dir,
    );

    refresh_entries(&remote, &mut app);

    let mut terminal = setup_terminal()?;
    let _terminal_cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    loop {
        terminal.draw(|frame| ui::render(&app, frame))?;

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match app.handle_key(key) {
                BrowserAction::Continue => {}
                BrowserAction::Quit => return Ok(VolumeBrowserOutput::Closed),
                BrowserAction::Refresh => refresh_entries(&remote, &mut app),
                BrowserAction::OpenSelected => open_selected(&remote, &mut app),
                BrowserAction::Parent => open_parent(&remote, &mut app),
                BrowserAction::DownloadSelected => {
                    if queue_download(&mut app) {
                        run_pending_transfer(&remote, &mut app, &mut terminal)
                    }
                }
                BrowserAction::StartUpload => app.set_status("Enter a local path to upload"),
                BrowserAction::SubmitUpload(path) => {
                    if queue_upload(&remote, &mut app, path) {
                        run_pending_transfer(&remote, &mut app, &mut terminal)
                    }
                }
                BrowserAction::ConfirmOverwrite => {
                    run_pending_transfer(&remote, &mut app, &mut terminal)
                }
                BrowserAction::CancelPrompt => {
                    app.pending_transfer = None;
                    app.set_status("Cancelled");
                }
            },
            Event::Resize(_, _) => terminal.clear()?,
            _ => {}
        }
    }
}

struct NativeRemote {
    service_instance_id: String,
    identity_file: Option<PathBuf>,
    control_path: PathBuf,
}

impl NativeRemote {
    fn connect(service_instance_id: String, identity_file: Option<PathBuf>) -> Result<Self> {
        let remote = Self {
            control_path: control_path(&service_instance_id),
            service_instance_id,
            identity_file,
        };
        remote.open_master_connection()?;
        Ok(remote)
    }

    fn target(&self) -> String {
        format!("{}@{}", self.service_instance_id, SSH_HOST)
    }

    fn base_ssh_command(&self) -> Command {
        let mut command = Command::new("ssh");
        if let Some(identity_file) = &self.identity_file {
            command.arg("-i").arg(identity_file);
        }
        command
    }

    fn open_master_connection(&self) -> Result<()> {
        let _ = fs::remove_file(&self.control_path);
        let status = self
            .base_ssh_command()
            .args([
                "-M",
                "-N",
                "-f",
                "-o",
                "ControlMaster=yes",
                "-o",
                &format!("ControlPath={}", self.control_path.display()),
                "-o",
                "ControlPersist=10m",
            ])
            .arg(self.target())
            .status()
            .context("Failed to start SSH control connection")?;

        if status.success() {
            Ok(())
        } else {
            bail!("Failed to start SSH control connection")
        }
    }

    fn ssh_command(&self) -> Command {
        let mut command = self.base_ssh_command();
        command
            .args([
                "-o",
                &format!("ControlPath={}", self.control_path.display()),
                "-o",
                "ControlMaster=no",
                "-o",
                "BatchMode=yes",
            ])
            .arg("-T");
        command.arg(self.target());
        command
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<RemoteEntry>> {
        let output = self
            .ssh_command()
            .arg(format!("unset LANG; ls -la {}/", shell_quote(path)))
            .output()
            .context("Failed to run ssh for remote directory listing")?;

        if !output.status.success() {
            bail!(
                "Remote directory listing failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        parse_ls_output(path, &String::from_utf8_lossy(&output.stdout))
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        let status = self
            .ssh_command()
            .arg(format!("test -e {}", shell_quote(path)))
            .status()
            .context("Failed to run ssh for remote path check")?;
        Ok(status.success())
    }

    fn remove_path(&self, path: &Path) -> Result<()> {
        let status = self
            .ssh_command()
            .arg(format!("rm -rf -- {}", shell_quote(path)))
            .status()
            .context("Failed to run ssh for remote path removal")?;
        if status.success() {
            Ok(())
        } else {
            bail!("Failed to remove remote path {}", path.display())
        }
    }

    fn download(&self, remote: &Path, local: &Path, is_dir: bool) -> Result<()> {
        if is_dir {
            self.download_dir(remote, local)
        } else {
            self.download_file(remote, local)
        }
    }

    fn upload(&self, local: &Path, remote: &Path) -> Result<()> {
        if local.is_dir() {
            self.upload_dir(local, remote)
        } else {
            self.upload_file(local, remote)
        }
    }

    fn download_file(&self, remote: &Path, local: &Path) -> Result<()> {
        let output = fs::File::create(local)
            .with_context(|| format!("Failed to create local file {}", local.display()))?;
        let status = self
            .ssh_command()
            .arg(format!("cat -- {}", shell_quote(remote)))
            .stdout(Stdio::from(output))
            .status()
            .context("Failed to run ssh for file download")?;

        if status.success() {
            Ok(())
        } else {
            bail!("Download failed for {}", remote.display())
        }
    }

    fn download_dir(&self, remote: &Path, local: &Path) -> Result<()> {
        let parent = remote
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Remote directory has no parent"))?;
        let name = remote
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Remote directory has no name"))?;
        let local_parent = local
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Local directory has no parent"))?;

        fs::create_dir_all(local_parent).with_context(|| {
            format!(
                "Failed to create local directory {}",
                local_parent.display()
            )
        })?;

        let mut ssh = self
            .ssh_command()
            .arg(format!(
                "tar -C {} -cf - {}",
                shell_quote(parent),
                shell_quote_path_fragment(name)
            ))
            .stdout(Stdio::piped())
            .spawn()
            .context("Failed to run ssh for directory download")?;

        let stdout = ssh
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to read remote tar stream"))?;

        let tar_status = Command::new("tar")
            .args(["-xf", "-", "-C"])
            .arg(local_parent)
            .stdin(Stdio::from(stdout))
            .status()
            .context("Failed to extract downloaded directory")?;

        let ssh_status = ssh
            .wait()
            .context("Failed to wait for remote directory download")?;

        if tar_status.success() && ssh_status.success() {
            Ok(())
        } else {
            bail!("Download failed for {}", remote.display())
        }
    }

    fn upload_file(&self, local: &Path, remote: &Path) -> Result<()> {
        let input = fs::File::open(local)
            .with_context(|| format!("Failed to open local file {}", local.display()))?;
        let status = self
            .ssh_command()
            .arg(format!("cat > {}", shell_quote(remote)))
            .stdin(Stdio::from(input))
            .status()
            .context("Failed to run ssh for file upload")?;

        if status.success() {
            Ok(())
        } else {
            bail!("Upload failed for {}", local.display())
        }
    }

    fn upload_dir(&self, local: &Path, remote: &Path) -> Result<()> {
        let local_parent = local
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Local directory has no parent"))?;
        let name = local
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Local directory has no name"))?;
        let remote_parent = remote
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Remote directory has no parent"))?;

        let mut tar = Command::new("tar")
            .args(["-C"])
            .arg(local_parent)
            .args(["-cf", "-"])
            .arg(name)
            .stdout(Stdio::piped())
            .spawn()
            .context("Failed to archive local directory for upload")?;

        let stdout = tar
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to read local tar stream"))?;

        let ssh_status = self
            .ssh_command()
            .arg(format!("tar -xf - -C {}", shell_quote(remote_parent)))
            .stdin(Stdio::from(stdout))
            .status()
            .context("Failed to run ssh for directory upload")?;

        let tar_status = tar
            .wait()
            .context("Failed to wait for local directory archive")?;

        if tar_status.success() && ssh_status.success() {
            Ok(())
        } else {
            bail!("Upload failed for {}", local.display())
        }
    }
}

impl Drop for NativeRemote {
    fn drop(&mut self) {
        let _ = self
            .base_ssh_command()
            .args([
                "-O",
                "exit",
                "-o",
                &format!("ControlPath={}", self.control_path.display()),
            ])
            .arg(self.target())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_file(&self.control_path);
    }
}

fn control_path(service_instance_id: &str) -> PathBuf {
    let service_fragment = service_instance_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>();
    env::temp_dir().join(format!(
        "railway-volume-{}-{}.sock",
        process::id(),
        service_fragment
    ))
}

fn refresh_entries(remote: &NativeRemote, app: &mut VolumeBrowserApp) {
    match remote.list_dir(&app.current_path) {
        Ok(entries) => {
            app.set_entries(entries);
            app.set_status("Ready");
        }
        Err(error) => app.set_error(format!("Failed to list directory: {error}")),
    }
}

fn open_selected(remote: &NativeRemote, app: &mut VolumeBrowserApp) {
    let Some(entry) = app.selected_entry().cloned() else {
        return;
    };

    if !entry.is_dir {
        app.set_status("Selected entry is not a directory");
        return;
    }

    app.current_path = entry.path;
    app.selected = 0;
    refresh_entries(remote, app);
}

fn open_parent(remote: &NativeRemote, app: &mut VolumeBrowserApp) {
    if app.current_path == app.mount_path {
        app.set_status("Already at volume mount path");
        return;
    }

    let Some(parent) = app.current_path.parent() else {
        return;
    };
    let parent = parent.to_path_buf();
    if !parent.starts_with(&app.mount_path) {
        app.set_status("Cannot navigate above volume mount path");
        return;
    }

    app.current_path = parent;
    app.selected = 0;
    refresh_entries(remote, app);
}

fn queue_download(app: &mut VolumeBrowserApp) -> bool {
    let Some(entry) = app.selected_entry().cloned() else {
        app.set_status("Nothing selected");
        return false;
    };

    let local = app.local_dir.join(&entry.name);
    app.pending_transfer = Some(PendingTransfer::Download {
        remote: entry,
        local: local.clone(),
    });

    if local.exists() {
        app.mode = app::BrowserMode::ConfirmOverwrite;
        false
    } else {
        true
    }
}

fn queue_upload(remote: &NativeRemote, app: &mut VolumeBrowserApp, input: PathBuf) -> bool {
    if input.as_os_str().is_empty() {
        app.set_status("Upload cancelled");
        return false;
    }

    let local = if input.is_absolute() {
        input
    } else {
        app.local_dir.join(input)
    };

    if !local.exists() {
        app.set_error(format!("Local path does not exist: {}", local.display()));
        return false;
    }

    let Some(name) = local.file_name() else {
        app.set_error("Local path must include a file or directory name");
        return false;
    };

    let remote_path = app.current_path.join(name);
    app.pending_transfer = Some(PendingTransfer::Upload {
        local,
        remote: remote_path.clone(),
    });

    match remote.exists(&remote_path) {
        Ok(true) => {
            app.mode = app::BrowserMode::ConfirmOverwrite;
            false
        }
        Ok(false) => true,
        Err(error) => {
            app.set_error(format!("Failed to check remote destination: {error}"));
            false
        }
    }
}

fn run_pending_transfer(
    remote: &NativeRemote,
    app: &mut VolumeBrowserApp,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) {
    let Some(transfer) = app.pending_transfer.take() else {
        return;
    };

    let result = run_with_restored_terminal(terminal, || match transfer {
        PendingTransfer::Download {
            remote: entry,
            local,
        } => {
            if local.exists() {
                remove_local_path(&local)?;
            }
            remote.download(&entry.path, &local, entry.is_dir)?;
            Ok(format!("Downloaded to {}", local.display()))
        }
        PendingTransfer::Upload {
            local,
            remote: dest,
        } => {
            if remote.exists(&dest)? {
                remote.remove_path(&dest)?;
            }
            remote.upload(&local, &dest)?;
            Ok(format!("Uploaded to {}", dest.display()))
        }
    });

    match result {
        Ok(message) => {
            app.set_status(message);
            refresh_entries(remote, app);
        }
        Err(error) => app.set_error(error.to_string()),
    }
}

fn run_with_restored_terminal<F>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    operation: F,
) -> Result<String>
where
    F: FnOnce() -> Result<String>,
{
    restore_terminal();
    let result = operation();
    let _ = execute!(stdout(), EnterAlternateScreen, Hide);
    let _ = crossterm::terminal::enable_raw_mode();
    let _ = terminal.clear();
    result
}

fn parse_ls_output(parent: &Path, output: &str) -> Result<Vec<RemoteEntry>> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("total ") {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(mode) = parts.next() else {
            continue;
        };

        for _ in 0..7 {
            parts.next();
        }

        let name = parts.collect::<Vec<_>>().join(" ");
        let name = name.split(" -> ").next().unwrap_or(name.as_str());
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }

        entries.push(RemoteEntry {
            path: parent.join(name),
            name: name.to_string(),
            is_dir: mode.starts_with('d'),
        });
    }

    Ok(entries)
}

fn shell_quote(path: &Path) -> String {
    let value = path.display().to_string();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_quote_path_fragment(value: &std::ffi::OsStr) -> String {
    let value = value.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn remove_local_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Failed to inspect local path {}", path.display()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("Failed to remove existing local path {}", path.display()))
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal() {
    let _ = execute!(stdout(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}
