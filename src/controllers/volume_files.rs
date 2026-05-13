use std::{
    env, fs,
    fs::File,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

const SSH_HOST: &str = "ssh.railway.com";

pub struct VolumeFileClient {
    service_instance_id: String,
    identity_file: Option<PathBuf>,
    control_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeFileEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: VolumeFileKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VolumeFileKind {
    File,
    Directory,
    Symlink,
    Other,
}

impl VolumeFileKind {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Directory => "[d]",
            Self::File => "[f]",
            Self::Symlink => "[l]",
            Self::Other => "[?]",
        }
    }

    pub fn is_dir(self) -> bool {
        self == Self::Directory
    }
}

impl VolumeFileClient {
    pub fn connect(service_instance_id: String, identity_file: Option<PathBuf>) -> Result<Self> {
        let client = Self {
            control_path: control_path(&service_instance_id),
            service_instance_id,
            identity_file,
        };
        client.open_master_connection()?;
        Ok(client)
    }

    pub fn list_dir(&self, path: &Path) -> Result<Vec<VolumeFileEntry>> {
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

        Ok(parse_ls_output(
            path,
            &String::from_utf8_lossy(&output.stdout),
        ))
    }

    pub fn exists(&self, path: &Path) -> Result<bool> {
        let status = self
            .ssh_command()
            .arg(format!("test -e {}", shell_quote(path)))
            .status()
            .context("Failed to run ssh for remote path check")?;
        Ok(status.success())
    }

    pub fn stat_kind(&self, path: &Path) -> Result<VolumeFileKind> {
        let output = self
            .ssh_command()
            .arg(format!(
                "if test -d {path}; then echo directory; elif test -f {path}; then echo file; elif test -L {path}; then echo symlink; elif test -e {path}; then echo other; else echo missing; exit 1; fi",
                path = shell_quote(path)
            ))
            .output()
            .context("Failed to run ssh for remote path stat")?;

        if !output.status.success() {
            bail!("Remote path does not exist: {}", path.display());
        }

        match String::from_utf8_lossy(&output.stdout).trim() {
            "directory" => Ok(VolumeFileKind::Directory),
            "file" => Ok(VolumeFileKind::File),
            "symlink" => Ok(VolumeFileKind::Symlink),
            _ => Ok(VolumeFileKind::Other),
        }
    }

    pub fn remove_path(&self, path: &Path) -> Result<()> {
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

    pub fn create_dir_all(&self, path: &Path) -> Result<()> {
        let status = self
            .ssh_command()
            .arg(format!("mkdir -p -- {}", shell_quote(path)))
            .status()
            .context("Failed to run ssh for remote directory creation")?;
        if status.success() {
            Ok(())
        } else {
            bail!("Failed to create remote directory {}", path.display())
        }
    }

    pub fn download(&self, remote: &Path, local: &Path, kind: VolumeFileKind) -> Result<()> {
        if kind.is_dir() {
            self.download_dir(remote, local)
        } else {
            self.download_file(remote, local)
        }
    }

    pub fn upload(&self, local: &Path, remote: &Path) -> Result<()> {
        if local.is_dir() {
            self.upload_dir(local, remote)
        } else {
            self.upload_file(local, remote)
        }
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
            .arg("-T")
            .arg(self.target());
        command
    }

    fn download_file(&self, remote: &Path, local: &Path) -> Result<()> {
        if let Some(parent) = local.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create local directory {}", parent.display())
            })?;
        }

        let file = File::create(local)
            .with_context(|| format!("Failed to create local file {}", local.display()))?;

        let status = self
            .ssh_command()
            .arg(format!("cat -- {}", shell_quote(remote)))
            .stdout(file)
            .status()
            .context("Failed to run ssh for file download")?;

        if status.success() {
            Ok(())
        } else {
            bail!("File download failed for {}", remote.display())
        }
    }

    fn download_dir(&self, remote: &Path, local: &Path) -> Result<()> {
        let remote_name = remote
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Remote directory has no name"))?;
        let remote_parent = remote
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Remote directory has no parent"))?;
        let local_parent = local
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Local directory has no parent"))?;

        fs::create_dir_all(local_parent).with_context(|| {
            format!(
                "Failed to create local directory {}",
                local_parent.display()
            )
        })?;

        let extracted_path = local_parent.join(remote_name);
        if extracted_path.exists() {
            remove_local_path(&extracted_path)?;
        }

        let mut remote_tar = self.ssh_command();
        remote_tar
            .arg(format!(
                "tar -C {} -cf - -- {}",
                shell_quote(remote_parent),
                shell_quote_path_fragment(remote_name)
            ))
            .stdout(Stdio::piped());

        let mut remote_tar = remote_tar
            .spawn()
            .context("Failed to start remote tar for directory download")?;
        let stdout = remote_tar
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture remote tar output"))?;

        let mut local_tar = Command::new("tar")
            .arg("-xf")
            .arg("-")
            .arg("-C")
            .arg(local_parent)
            .stdin(stdout)
            .spawn()
            .context("Failed to start local tar for directory download")?;

        let local_status = local_tar
            .wait()
            .context("Failed waiting for local tar during directory download")?;
        let remote_status = remote_tar
            .wait()
            .context("Failed waiting for remote tar during directory download")?;

        if !remote_status.success() || !local_status.success() {
            bail!("Directory download failed for {}", remote.display());
        }

        if extracted_path != local {
            fs::rename(&extracted_path, local).with_context(|| {
                format!(
                    "Failed to move downloaded directory from {} to {}",
                    extracted_path.display(),
                    local.display()
                )
            })?;
        }

        Ok(())
    }

    fn upload_file(&self, local: &Path, remote: &Path) -> Result<()> {
        if let Some(parent) = remote.parent() {
            self.create_dir_all(parent)?;
        }

        let file = File::open(local)
            .with_context(|| format!("Failed to open local file {}", local.display()))?;

        let status = self
            .ssh_command()
            .arg(format!("cat > {}", shell_quote(remote)))
            .stdin(file)
            .status()
            .context("Failed to run ssh for file upload")?;

        if status.success() {
            Ok(())
        } else {
            bail!("File upload failed for {}", local.display())
        }
    }

    fn upload_dir(&self, local: &Path, remote: &Path) -> Result<()> {
        let local_name = local
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Local directory has no name"))?;
        let local_parent = local
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Local directory has no parent"))?;
        let remote_parent = remote
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Remote directory has no parent"))?;

        self.create_dir_all(remote_parent)?;

        let extracted_path = remote_parent.join(local_name);
        if extracted_path != remote && self.exists(&extracted_path)? {
            self.remove_path(&extracted_path)?;
        }

        let mut local_tar = Command::new("tar")
            .arg("-C")
            .arg(local_parent)
            .arg("-cf")
            .arg("-")
            .arg(local_name)
            .stdout(Stdio::piped())
            .spawn()
            .context("Failed to start local tar for directory upload")?;
        let stdout = local_tar
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture local tar output"))?;

        let mut remote_tar = self.ssh_command();
        remote_tar
            .arg(format!("tar -xf - -C {}", shell_quote(remote_parent)))
            .stdin(stdout);

        let mut remote_tar = remote_tar
            .spawn()
            .context("Failed to start remote tar for directory upload")?;

        let local_status = local_tar
            .wait()
            .context("Failed waiting for local tar during directory upload")?;
        let remote_status = remote_tar
            .wait()
            .context("Failed waiting for remote tar during directory upload")?;

        if !local_status.success() || !remote_status.success() {
            bail!("Directory upload failed for {}", local.display());
        }

        if extracted_path != remote {
            let status = self
                .ssh_command()
                .arg(format!(
                    "mv -- {} {}",
                    shell_quote(&extracted_path),
                    shell_quote(remote)
                ))
                .status()
                .context("Failed to rename uploaded remote directory")?;
            if !status.success() {
                bail!(
                    "Failed to move uploaded directory from {} to {}",
                    extracted_path.display(),
                    remote.display()
                );
            }
        }

        Ok(())
    }
}

impl Drop for VolumeFileClient {
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

fn parse_ls_output(parent: &Path, output: &str) -> Vec<VolumeFileEntry> {
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

        let kind = match mode.chars().next() {
            Some('d') => VolumeFileKind::Directory,
            Some('-') => VolumeFileKind::File,
            Some('l') => VolumeFileKind::Symlink,
            _ => VolumeFileKind::Other,
        };

        entries.push(VolumeFileEntry {
            path: parent.join(name),
            name: name.to_string(),
            kind,
        });
    }

    entries
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

fn shell_quote(path: &Path) -> String {
    let value = path.display().to_string();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_quote_path_fragment(fragment: &std::ffi::OsStr) -> String {
    let value = fragment.to_string_lossy();
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
