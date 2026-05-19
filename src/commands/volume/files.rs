use std::path::PathBuf;

use anyhow::bail;
use clap::Parser;
use colored::Colorize;
use is_terminal::IsTerminal;

use crate::{
    commands::volume::sftp::{self, VolumeSftp},
    controllers::volume_browser::{self, VolumeBrowserParams},
    telemetry,
    util::prompt::prompt_confirm_with_default,
};

use super::super::Result;

#[derive(Clone)]
pub(crate) struct FileTarget {
    pub(crate) service_instance_id: String,
    pub(crate) mount_path: String,
    pub(crate) label: FileTargetLabel,
}

#[derive(Clone)]
pub(crate) enum FileTargetLabel {
    Volume {
        id: String,
        name: String,
        mount_path: String,
    },
    Service {
        id: String,
        name: String,
    },
}

#[derive(Parser)]
pub(crate) enum Commands {
    /// Download a file or directory
    Download(DownloadArgs),

    /// Upload a file or directory
    Upload(UploadArgs),

    /// List files in a directory
    #[clap(visible_alias = "ls")]
    List(ListArgs),

    /// Browse files interactively
    Browse(BrowseArgs),

    /// Delete a file
    #[clap(visible_alias = "rm", visible_alias = "remove")]
    Delete(DeleteArgs),

    /// Rename a file
    #[clap(visible_alias = "mv")]
    Rename(RenameArgs),
}

#[derive(Parser)]
pub(crate) struct DownloadArgs {
    /// The path on the remote server to download from
    #[clap(value_name = "REMOTE_PATH")]
    pub(crate) remote_path: String,

    /// The path to save the download
    #[clap(value_name = "LOCAL_PATH", default_value = ".")]
    pub(crate) local_path: PathBuf,

    /// Output in JSON format
    #[clap(long)]
    pub(crate) json: bool,

    /// Replace LOCAL_PATH if it already exists
    #[clap(long, visible_alias = "override")]
    pub(crate) overwrite: bool,

    /// Concurrent file downloads when REMOTE_PATH is a directory
    #[clap(long, value_name = "N", default_value_t = sftp::DEFAULT_TRANSFER_CONCURRENCY)]
    pub(crate) concurrency: usize,
}

#[derive(Parser)]
pub(crate) struct UploadArgs {
    /// The local file or directory to upload
    #[clap(value_name = "LOCAL_PATH")]
    pub(crate) local_path: PathBuf,

    /// The path on the remote server to upload to
    #[clap(value_name = "REMOTE_PATH")]
    pub(crate) remote_path: String,

    /// Output in JSON format
    #[clap(long)]
    pub(crate) json: bool,

    /// Replace REMOTE_PATH if it already exists
    #[clap(long)]
    pub(crate) overwrite: bool,

    /// Concurrent file uploads when LOCAL_PATH is a directory
    #[clap(long, value_name = "N", default_value_t = sftp::DEFAULT_TRANSFER_CONCURRENCY)]
    pub(crate) concurrency: usize,
}

#[derive(Parser)]
pub(crate) struct ListArgs {
    /// The directory path on the remote server to list
    #[clap(value_name = "REMOTE_PATH", default_value = "/")]
    pub(crate) remote_path: String,

    /// Output in JSON format
    #[clap(long)]
    pub(crate) json: bool,
}

#[derive(Parser)]
pub(crate) struct BrowseArgs {
    /// The directory path on the remote server to open
    #[clap(value_name = "REMOTE_PATH", default_value = "/")]
    pub(crate) remote_path: String,

    /// Concurrent file downloads
    #[clap(long, value_name = "N", default_value_t = sftp::DEFAULT_TRANSFER_CONCURRENCY)]
    pub(crate) concurrency: usize,
}

#[derive(Parser)]
pub(crate) struct DeleteArgs {
    /// The path on the remote server to delete
    #[clap(value_name = "REMOTE_PATH")]
    pub(crate) remote_path: String,

    /// Output in JSON format
    #[clap(long)]
    pub(crate) json: bool,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    pub(crate) yes: bool,
}

#[derive(Parser)]
pub(crate) struct RenameArgs {
    /// The current path on the remote server
    #[clap(value_name = "OLD_REMOTE_PATH")]
    pub(crate) old_remote_path: String,

    /// The new path on the remote server
    #[clap(value_name = "NEW_REMOTE_PATH")]
    pub(crate) new_remote_path: String,

    /// Output in JSON format
    #[clap(long)]
    pub(crate) json: bool,
}

pub(crate) async fn command_from_parts(target: FileTarget, command: Commands) -> Result<()> {
    match command {
        Commands::Download(args) => download(target, args).await,
        Commands::Upload(args) => upload(target, args).await,
        Commands::List(args) => list(target, args).await,
        Commands::Browse(args) => browse(target, args).await,
        Commands::Delete(args) => delete(target, args).await,
        Commands::Rename(args) => rename(target, args).await,
    }
}

pub(crate) async fn download(target: FileTarget, args: DownloadArgs) -> Result<()> {
    let mut sftp = sftp_for(&target, args.concurrency);
    let downloaded_path = sftp
        .download(&args.remote_path, &args.local_path, args.overwrite)
        .await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&target_json(
                &target,
                serde_json::json!({
                    "remotePath": args.remote_path,
                    "localPath": downloaded_path,
                    "overwritten": args.overwrite,
                }),
            ))?
        );
    } else {
        println!(
            "Downloaded {} to {}",
            args.remote_path.cyan(),
            downloaded_path.display().to_string().green()
        );
    }

    Ok(())
}

pub(crate) async fn upload(target: FileTarget, args: UploadArgs) -> Result<()> {
    let mut sftp = sftp_for(&target, args.concurrency);
    let uploaded_path = sftp
        .upload(&args.local_path, &args.remote_path, args.overwrite)
        .await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&target_json(
                &target,
                serde_json::json!({
                    "localPath": args.local_path,
                    "remotePath": uploaded_path,
                    "overwritten": args.overwrite,
                }),
            ))?
        );
    } else {
        println!(
            "Uploaded {} to {}",
            args.local_path.display().to_string().cyan(),
            uploaded_path.green()
        );
    }

    Ok(())
}

pub(crate) async fn list(target: FileTarget, args: ListArgs) -> Result<()> {
    let mut sftp = sftp_for(&target, sftp::DEFAULT_TRANSFER_CONCURRENCY);
    let file_tree = sftp.list_files(&args.remote_path).await?;

    if args.json {
        let files: Vec<serde_json::Value> = file_tree
            .entries()
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name,
                    "path": entry.path,
                    "type": entry.kind,
                    "size": entry.size,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&target_json(
                &target,
                serde_json::json!({
                    "remotePath": args.remote_path,
                    "files": files,
                }),
            ))?
        );
    } else {
        print!("{file_tree}");
    }

    Ok(())
}

pub(crate) async fn browse(target: FileTarget, args: BrowseArgs) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!("The browse command requires an interactive terminal");
    }

    volume_browser::run(VolumeBrowserParams {
        service_instance_id: target.service_instance_id.clone(),
        volume_name: target.name(),
        mount_path: target.mount_path.clone(),
        remote_path: args.remote_path,
        transfer_concurrency: args.concurrency,
    })
    .await
}

pub(crate) async fn delete(target: FileTarget, args: DeleteArgs) -> Result<()> {
    if telemetry::is_agent() {
        bail!("{}", agent_file_delete_refusal(&target, &args.remote_path));
    }

    let is_terminal = std::io::stdout().is_terminal();
    let confirm = if args.yes {
        true
    } else if is_terminal {
        prompt_confirm_with_default(
            format!(r#"Are you sure you want to delete "{}"?"#, args.remote_path).as_str(),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirm {
        return Ok(());
    }

    let mut sftp = sftp_for(&target, sftp::DEFAULT_TRANSFER_CONCURRENCY);
    sftp.delete_file(&args.remote_path).await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&target_json(
                &target,
                serde_json::json!({
                    "remotePath": args.remote_path,
                    "deleted": true,
                }),
            ))?
        );
    } else {
        println!("Deleted {}", args.remote_path.cyan());
    }

    Ok(())
}

pub(crate) async fn rename(target: FileTarget, args: RenameArgs) -> Result<()> {
    let mut sftp = sftp_for(&target, sftp::DEFAULT_TRANSFER_CONCURRENCY);
    sftp.rename(&args.old_remote_path, &args.new_remote_path)
        .await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&target_json(
                &target,
                serde_json::json!({
                    "oldRemotePath": args.old_remote_path,
                    "newRemotePath": args.new_remote_path,
                    "renamed": true,
                }),
            ))?
        );
    } else {
        println!(
            "Renamed {} to {}",
            args.old_remote_path.cyan(),
            args.new_remote_path.green()
        );
    }

    Ok(())
}

fn sftp_for(target: &FileTarget, concurrency: usize) -> VolumeSftp {
    let mut sftp = VolumeSftp::new(
        target.service_instance_id.clone(),
        target.mount_path.clone(),
    );
    sftp.set_transfer_concurrency(concurrency);
    sftp
}

fn target_json(target: &FileTarget, details: serde_json::Value) -> serde_json::Value {
    let mut output = match &target.label {
        FileTargetLabel::Volume {
            id,
            name,
            mount_path,
        } => serde_json::json!({
            "volume": {
                "id": id,
                "name": name,
                "mountPath": mount_path,
            },
            "serviceInstanceId": target.service_instance_id,
        }),
        FileTargetLabel::Service { id, name } => serde_json::json!({
            "service": {
                "id": id,
                "name": name,
            },
            "serviceInstanceId": target.service_instance_id,
        }),
    };

    if let (Some(output), Some(details)) = (output.as_object_mut(), details.as_object()) {
        for (key, value) in details {
            output.insert(key.clone(), value.clone());
        }
    }

    output
}

fn agent_file_delete_refusal(target: &FileTarget, remote_path: &str) -> String {
    let command = human_delete_file_command(target, remote_path);
    format!("Refusing: agents cannot delete files. Ask a human to run:\n\n  {command}")
}

fn human_delete_file_command(target: &FileTarget, remote_path: &str) -> String {
    let mut command = match &target.label {
        FileTargetLabel::Volume { name, .. } => {
            format!("railway volume files delete --volume {}", shell_quote(name))
        }
        FileTargetLabel::Service { name, .. } => {
            format!(
                "railway service files delete --service {}",
                shell_quote(name)
            )
        }
    };
    command.push(' ');
    command.push_str(&shell_quote(remote_path));
    command
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

impl FileTarget {
    fn name(&self) -> String {
        match &self.label {
            FileTargetLabel::Volume { name, .. } | FileTargetLabel::Service { name, .. } => {
                name.clone()
            }
        }
    }
}
