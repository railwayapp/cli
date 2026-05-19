use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use is_terminal::IsTerminal;
use regex::{NoExpand, Regex};
use tempfile::NamedTempFile;

use crate::{client::GQLClient, config::Configs, errors::RailwayError};

use super::{common::get_ssh_connect_params, native};

/// Emit or manage an OpenSSH config block for a Railway service
#[derive(Parser, Clone)]
pub struct Args {
    /// Project to connect to (defaults to linked project)
    #[clap(short, long)]
    project: Option<String>,

    /// Service to connect to (defaults to linked service).
    /// With --remove, this can remove a local marker by service name without resolving Railway.
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to connect to (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Host alias to use in the SSH config
    #[clap(long)]
    alias: Option<String>,

    /// Emit an IdentityFile directive for this private key path
    #[clap(short = 'i', long = "identity-file", value_name = "PATH")]
    identity_file: Option<PathBuf>,

    /// Insert or update the generated block in the SSH config file
    #[clap(long, conflicts_with = "remove")]
    write: bool,

    /// Remove the generated block from the SSH config file
    #[clap(long, conflicts_with = "write")]
    remove: bool,

    /// SSH config file to update
    #[clap(long, default_value = "~/.ssh/config")]
    path: PathBuf,
}

struct ResolvedConfig {
    service_name: String,
    alias: String,
    service_instance_id: String,
}

pub async fn command(args: Args) -> Result<()> {
    ensure_default_target_has_linked_service(&args).await?;

    if args.remove {
        let service_name = if let Some(service_name) = args.service.as_deref() {
            service_name.to_string()
        } else {
            resolve_service_name(args.clone()).await?
        };

        let path = expand_tilde(&args.path)?;
        let removed = remove_config_block(&path, &service_name)?;
        if removed {
            eprintln!("Removed Railway SSH config block from {}", path.display());
        } else {
            eprintln!(
                "No Railway SSH config block found for {} in {}",
                service_name,
                path.display()
            );
        }
        return Ok(());
    }

    let resolved = resolve(args.clone()).await?;
    let block = render_config_block(
        &resolved.service_name,
        &resolved.alias,
        &resolved.service_instance_id,
        args.identity_file.as_deref(),
        &Utc::now().format("%Y-%m-%d").to_string(),
    );

    if args.write {
        let path = expand_tilde(&args.path)?;
        upsert_config_block(&path, &resolved.service_name, &block)?;
        eprintln!("Wrote Railway SSH config block to {}", path.display());
    } else {
        print!("{block}");
    }

    Ok(())
}

async fn ensure_default_target_has_linked_service(args: &Args) -> Result<()> {
    if args.remove || args.project.is_some() || args.environment.is_some() || args.service.is_some()
    {
        return Ok(());
    }
    if !std::io::stdout().is_terminal() {
        return Ok(());
    }

    let configs = Configs::new()?;
    match configs.get_linked_project().await {
        Ok(linked_project) if linked_project.service.is_some() => Ok(()),
        Ok(_) => crate::commands::service::link_current_project_service(None).await,
        Err(error) => {
            if error
                .downcast_ref::<RailwayError>()
                .is_some_and(|error| matches!(error, RailwayError::NoLinkedProject))
            {
                crate::commands::link::command_requiring_service(
                    crate::commands::link::Args::for_service_link(None, None, None),
                )
                .await
            } else {
                Err(error)
            }
        }
    }
}

async fn resolve(args: Args) -> Result<ResolvedConfig> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let params =
        get_ssh_connect_params(ssh_args_from_config_args(&args), &configs, &client).await?;

    let alias = args
        .alias
        .as_deref()
        .map(sanitize_alias)
        .unwrap_or_else(|| format!("railway-{}", sanitize_alias(&params.service_name)));

    let service_instance_id = native::get_service_instance_id(
        &client,
        &configs,
        &params.environment_id,
        &params.service_id,
    )
    .await?;

    Ok(ResolvedConfig {
        service_name: params.service_name,
        alias,
        service_instance_id,
    })
}

async fn resolve_service_name(args: Args) -> Result<String> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let params =
        get_ssh_connect_params(ssh_args_from_config_args(&args), &configs, &client).await?;

    Ok(params.service_name)
}

fn ssh_args_from_config_args(args: &Args) -> super::Args {
    super::Args {
        subcommand: None,
        project: args.project.clone(),
        service: args.service.clone(),
        environment: args.environment.clone(),
        deployment_instance: None,
        session: None,
        native: false,
        identity_file: None,
        command: Vec::new(),
    }
}

fn render_config_block(
    service_name: &str,
    alias: &str,
    service_instance_id: &str,
    identity_file: Option<&Path>,
    date: &str,
) -> String {
    let marker_name = marker_name(service_name);
    let mut block = String::new();
    writeln!(
        block,
        "# BEGIN railway:{marker_name} ----- written {date} by `railway ssh config`"
    )
    .expect("writing to String cannot fail");
    writeln!(
        block,
        "# If you rename, delete, or re-add this service, re-run this command."
    )
    .expect("writing to String cannot fail");
    writeln!(block, "Host {alias}").expect("writing to String cannot fail");
    writeln!(block, "    HostName {}", native::SSH_HOST).expect("writing to String cannot fail");
    writeln!(block, "    User {service_instance_id}").expect("writing to String cannot fail");

    if let Some(identity_file) = identity_file {
        writeln!(
            block,
            "    IdentityFile {}",
            quote_ssh_config_value(&identity_file.to_string_lossy())
        )
        .expect("writing to String cannot fail");
    }

    writeln!(block, "    ServerAliveInterval 30").expect("writing to String cannot fail");
    writeln!(block, "    ServerAliveCountMax 3").expect("writing to String cannot fail");
    writeln!(block, "# END railway:{marker_name}").expect("writing to String cannot fail");

    block
}

fn upsert_config_block(path: &Path, service_name: &str, block: &str) -> Result<()> {
    let existing = read_config(path)?;
    let pattern = config_block_regex_with_case(service_name, false, true)?;
    let updated = if pattern.is_match(&existing) {
        pattern.replace(&existing, NoExpand(block)).into_owned()
    } else {
        append_config_block(existing, block)
    };

    write_config(path, &updated)
}

fn remove_config_block(path: &Path, service_name: &str) -> Result<bool> {
    let existing = read_config(path)?;
    let pattern = config_block_regex_with_case(service_name, true, true)?;

    if !pattern.is_match(&existing) {
        return Ok(false);
    }

    let updated = pattern.replace(&existing, "").into_owned();
    write_config(path, &updated)?;

    Ok(true)
}

fn read_config(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn write_config(path: &Path, contents: &str) -> Result<()> {
    let parent = writable_parent(path);
    fs::create_dir_all(parent).with_context(|| format!("Failed to create {}", parent.display()))?;
    secure_ssh_dir(parent)?;

    let mut temp_file = NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temporary file in {}", parent.display()))?;
    temp_file
        .write_all(contents.as_bytes())
        .context("Failed to write temporary SSH config")?;
    temp_file
        .as_file_mut()
        .sync_all()
        .context("Failed to sync temporary SSH config")?;
    secure_file(temp_file.path())?;
    temp_file
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    secure_file(path)?;

    Ok(())
}

fn writable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(unix)]
fn secure_ssh_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if path.file_name().and_then(|name| name.to_str()) == Some(".ssh") {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("Failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}

#[cfg(not(unix))]
fn secure_ssh_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("Failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> Result<()> {
    Ok(())
}

fn append_config_block(mut existing: String, block: &str) -> String {
    if !existing.is_empty() {
        if !existing.ends_with('\n') {
            existing.push('\n');
        }

        if !existing.ends_with("\n\n") {
            existing.push('\n');
        }
    }

    existing.push_str(block);
    existing
}

fn config_block_regex_with_case(
    service_name: &str,
    include_preceding_blank: bool,
    case_insensitive: bool,
) -> Result<Regex> {
    let marker = regex::escape(&marker_name(service_name));
    let preceding_blank = if include_preceding_blank {
        r"(?:^[ \t]*\r?\n)?"
    } else {
        ""
    };
    let flags = if case_insensitive { "(?ims)" } else { "(?ms)" };
    Regex::new(&format!(
        r"{flags}{preceding_blank}^# BEGIN railway:{marker}(?: .*)?\r?\n.*?^# END railway:{marker}[ \t]*(?:\r?\n)?"
    ))
    .context("Failed to build Railway SSH config marker regex")
}

fn marker_name(service_name: &str) -> String {
    service_name.replace(['\r', '\n'], " ")
}

fn sanitize_alias(input: &str) -> String {
    let mut sanitized = String::new();
    let mut previous_was_dash = false;

    for byte in input.bytes() {
        let c = byte.to_ascii_lowercase() as char;
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_') {
            sanitized.push(c);
            previous_was_dash = false;
        } else if c == '-' {
            if !previous_was_dash {
                sanitized.push(c);
            }
            previous_was_dash = true;
        } else if !previous_was_dash {
            sanitized.push('-');
            previous_was_dash = true;
        }
    }

    let sanitized = sanitized.trim_matches('-').to_string();
    if sanitized.is_empty() {
        "service".to_string()
    } else {
        sanitized
    }
}

fn quote_ssh_config_value(value: &str) -> String {
    if !value
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\\'))
    {
        return value.to_string();
    }

    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');

    for c in value.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            _ => quoted.push(c),
        }
    }

    quoted.push('"');
    quoted
}

fn expand_tilde(path: &Path) -> Result<PathBuf> {
    let path = path.to_string_lossy();

    if path == "~" {
        return dirs::home_dir().context("Could not find home directory");
    }

    for prefix in ["~/", "~\\"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            return dirs::home_dir()
                .map(|home| home.join(rest))
                .context("Could not find home directory");
        }
    }

    Ok(PathBuf::from(path.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_aliases() {
        assert_eq!(sanitize_alias("API Service"), "api-service");
        assert_eq!(sanitize_alias("railway.API_01"), "railway.api_01");
        assert_eq!(sanitize_alias("!!!"), "service");
    }

    #[test]
    fn write_upserts_block_idempotently_and_sets_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ssh/config");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "Host existing\n    HostName example.com\n").unwrap();

        let old_block = render_config_block("API", "railway-api", "old-id", None, "2026-05-17");
        let new_block = render_config_block(
            "api",
            "railway-api",
            "new-id",
            Some(Path::new("/Users/me/My Keys/id_ed25519")),
            "2026-05-18",
        );

        upsert_config_block(&path, "API", &old_block).unwrap();
        upsert_config_block(&path, "api", &new_block).unwrap();
        upsert_config_block(&path, "api", &new_block).unwrap();

        let contents = fs::read_to_string(&path).unwrap();

        assert_eq!(contents.matches("# BEGIN railway:").count(), 1);
        assert!(contents.starts_with("Host existing\n    HostName example.com\n\n"));
        assert!(contents.contains("# BEGIN railway:api ----- written 2026-05-18"));
        assert!(contents.contains("Host railway-api\n"));
        assert!(contents.contains("    HostName ssh.railway.com\n"));
        assert!(contents.contains("    User new-id\n"));
        assert!(contents.contains("    IdentityFile \"/Users/me/My Keys/id_ed25519\"\n"));
        assert!(!contents.contains("old-id"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let ssh_mode = fs::metadata(dir.path().join(".ssh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;

            assert_eq!(ssh_mode, 0o700);
            assert_eq!(file_mode, 0o600);
        }
    }

    #[tokio::test]
    async fn remove_with_service_arg_deletes_marked_block_without_resolving() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ssh/config");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "\
Host existing

# BEGIN railway:API ----- written 2026-01-01 by `railway ssh config`
Host old
# END railway:API
Host later
",
        )
        .unwrap();

        command(Args {
            project: None,
            service: Some("api".to_string()),
            environment: None,
            alias: None,
            identity_file: None,
            write: false,
            remove: true,
            path: path.clone(),
        })
        .await
        .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "Host existing\nHost later\n"
        );

        command(Args {
            project: None,
            service: Some("API".to_string()),
            environment: None,
            alias: None,
            identity_file: None,
            write: false,
            remove: true,
            path,
        })
        .await
        .unwrap();
    }
}
