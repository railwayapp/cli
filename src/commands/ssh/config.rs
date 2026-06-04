use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Args as ClapArgs, Parser, Subcommand};
use is_terminal::IsTerminal;
use regex::{NoExpand, Regex};
use tempfile::NamedTempFile;

use crate::{client::GQLClient, config::Configs, errors::RailwayError};

use super::{common::get_ssh_connect_params, native};

/// Add, preview, or remove an OpenSSH config block for a Railway service
#[derive(Parser, Clone)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    #[clap(flatten)]
    target: TargetArgs,

    /// Host alias to use in the SSH config
    #[clap(long)]
    alias: Option<String>,

    /// Emit an IdentityFile directive for this private key path
    #[clap(short = 'i', long = "identity-file", value_name = "PATH")]
    identity_file: Option<PathBuf>,

    /// Print the generated block without writing the SSH config file
    #[clap(long)]
    dry_run: bool,
}

#[derive(Subcommand, Clone)]
enum Commands {
    /// Remove the Railway block from the SSH config file
    #[clap(visible_alias = "rm")]
    Remove,
}

#[derive(ClapArgs, Clone)]
struct TargetArgs {
    /// Project to use (defaults to linked project)
    #[clap(short, long, global = true)]
    project: Option<String>,

    /// Service to use (defaults to linked service).
    /// With remove, this can remove a local marker by service name without resolving Railway.
    #[clap(short, long, global = true)]
    service: Option<String>,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long, global = true)]
    environment: Option<String>,

    /// SSH config file to update or remove from
    #[clap(long, default_value = "~/.ssh/config", global = true)]
    path: PathBuf,
}

impl Default for TargetArgs {
    fn default() -> Self {
        Self {
            project: None,
            service: None,
            environment: None,
            path: PathBuf::from("~/.ssh/config"),
        }
    }
}

struct ResolvedConfig {
    service_name: String,
    config_marker: String,
    alias: String,
    service_instance_id: String,
}

pub async fn command(args: Args) -> Result<()> {
    if let Some(Commands::Remove) = args.command {
        if args.alias.is_some() {
            bail!("--alias cannot be used with remove");
        }
        if args.identity_file.is_some() {
            bail!("--identity-file cannot be used with remove");
        }
        if args.dry_run {
            bail!("--dry-run cannot be used with remove");
        }

        return remove_command(args.target).await;
    }

    ensure_default_target_has_linked_service(&args.target).await?;

    let resolved = resolve(&args.target, args.alias.as_deref()).await?;
    let block = render_config_block(
        &resolved.service_name,
        &resolved.config_marker,
        &resolved.alias,
        &resolved.service_instance_id,
        args.identity_file.as_deref(),
    );

    if args.dry_run {
        print!("{block}");
    } else {
        let path = expand_tilde(&args.target.path)?;
        upsert_config_block(
            &path,
            &resolved.config_marker,
            &resolved.service_name,
            &block,
        )?;
        eprintln!("Wrote Railway SSH config block to {}", path.display());
    }

    Ok(())
}

async fn remove_command(target: TargetArgs) -> Result<()> {
    let path = expand_tilde(&target.path)?;
    let should_resolve_target =
        target.service.is_none() || target.project.is_some() || target.environment.is_some();
    let (service_name, removed) = if let Some(service_name) =
        target.service.as_deref().filter(|_| !should_resolve_target)
    {
        (
            service_name.to_string(),
            remove_config_blocks_by_service_name(&path, service_name)?,
        )
    } else {
        let resolved = resolve_target(&target).await?;
        let removed =
            remove_config_block_for_target(&path, &resolved.config_marker, &resolved.service_name)?;
        (resolved.service_name, removed)
    };

    if removed {
        eprintln!("Removed Railway SSH config block from {}", path.display());
    } else {
        eprintln!(
            "No Railway SSH config block found for {} in {}",
            service_name,
            path.display()
        );
    }

    Ok(())
}

async fn ensure_default_target_has_linked_service(target: &TargetArgs) -> Result<()> {
    if target.project.is_some() || target.environment.is_some() || target.service.is_some() {
        return Ok(());
    }
    if !std::io::stdout().is_terminal() {
        return Ok(());
    }
    if Configs::has_env_var_project_config() || Configs::get_railway_token().is_some() {
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

async fn resolve(target: &TargetArgs, alias: Option<&str>) -> Result<ResolvedConfig> {
    let (configs, client, params, config_marker) = resolve_params(target).await?;

    let alias = alias
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
        config_marker,
        alias,
        service_instance_id,
    })
}

struct ResolvedTarget {
    service_name: String,
    config_marker: String,
}

async fn resolve_target(target: &TargetArgs) -> Result<ResolvedTarget> {
    let (_configs, _client, params, config_marker) = resolve_params(target).await?;

    Ok(ResolvedTarget {
        service_name: params.service_name,
        config_marker,
    })
}

async fn resolve_params(
    target: &TargetArgs,
) -> Result<(
    Configs,
    reqwest::Client,
    super::common::SshConnectParams,
    String,
)> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let params =
        get_ssh_connect_params(ssh_args_from_target_args(target), &configs, &client).await?;
    let config_marker = target_marker(
        &params.project_id,
        &params.environment_id,
        &params.service_id,
    );

    Ok((configs, client, params, config_marker))
}

fn ssh_args_from_target_args(target: &TargetArgs) -> super::Args {
    super::Args {
        subcommand: None,
        project: target.project.clone(),
        service: target.service.clone(),
        environment: target.environment.clone(),
        deployment_instance: None,
        session: None,
        native: false,
        identity_file: None,
        command: Vec::new(),
    }
}

fn render_config_block(
    service_name: &str,
    config_marker: &str,
    alias: &str,
    service_instance_id: &str,
    identity_file: Option<&Path>,
) -> String {
    let rendered_marker = marker_name(config_marker);
    let rendered_service_name = marker_name(service_name);
    let mut block = String::new();

    writeln!(block, "# BEGIN railway:{rendered_marker}").expect("writing to String cannot fail");
    writeln!(block, "# Railway service: {rendered_service_name}")
        .expect("writing to String cannot fail");
    let (relay_host, relay_port) = native::ssh_relay();
    writeln!(block, "Host {alias}").expect("writing to String cannot fail");
    writeln!(block, "    HostName {relay_host}").expect("writing to String cannot fail");
    if let Some(port) = relay_port {
        writeln!(block, "    Port {port}").expect("writing to String cannot fail");
    }
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
    writeln!(block, "# END railway:{rendered_marker}").expect("writing to String cannot fail");

    block
}

fn upsert_config_block(
    path: &Path,
    config_marker: &str,
    service_name: &str,
    block: &str,
) -> Result<()> {
    let existing = read_config(path)?;
    let pattern = config_block_regex_with_case(config_marker, false, true)?;
    let updated = if pattern.is_match(&existing) {
        pattern.replace(&existing, NoExpand(block)).into_owned()
    } else {
        let legacy_pattern = config_block_regex_with_case(service_name, false, true)?;
        if legacy_pattern.is_match(&existing) {
            legacy_pattern
                .replace(&existing, NoExpand(block))
                .into_owned()
        } else {
            append_config_block(existing, block)
        }
    };

    write_config(path, &updated)
}

fn remove_config_block_for_target(
    path: &Path,
    config_marker: &str,
    service_name: &str,
) -> Result<bool> {
    let existing = read_config(path)?;
    let pattern = config_block_regex_with_case(config_marker, true, true)?;

    let updated = if pattern.is_match(&existing) {
        pattern.replace(&existing, "").into_owned()
    } else {
        let legacy_pattern = config_block_regex_with_case(service_name, true, true)?;
        if !legacy_pattern.is_match(&existing) {
            return Ok(false);
        }
        legacy_pattern.replace(&existing, "").into_owned()
    };

    write_config(path, &updated)?;

    Ok(true)
}

fn remove_config_blocks_by_service_name(path: &Path, service_name: &str) -> Result<bool> {
    let existing = read_config(path)?;
    let pattern = any_config_block_regex(true)?;
    let mut removed = false;

    let updated = pattern
        .replace_all(&existing, |captures: &regex::Captures<'_>| {
            let block = captures.get(0).map_or("", |matched| matched.as_str());
            if block_matches_service_name(block, service_name) {
                removed = true;
                String::new()
            } else {
                block.to_string()
            }
        })
        .into_owned();

    if !removed {
        return Ok(false);
    }

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
    config_marker: &str,
    include_preceding_blank: bool,
    case_insensitive: bool,
) -> Result<Regex> {
    let marker = regex::escape(&marker_name(config_marker));
    let preceding_blank = if include_preceding_blank {
        r"(?:^[ \t]*\r?\n)?"
    } else {
        ""
    };
    let flags = if case_insensitive { "(?ims)" } else { "(?ms)" };
    Regex::new(&format!(
        r"{flags}{preceding_blank}^# BEGIN railway:{marker}[ \t]*\r?\n.*?^# END railway:{marker}[ \t]*(?:\r?\n)?"
    ))
    .context("Failed to build Railway SSH config marker regex")
}

fn any_config_block_regex(include_preceding_blank: bool) -> Result<Regex> {
    let preceding_blank = if include_preceding_blank {
        r"(?:^[ \t]*\r?\n)?"
    } else {
        ""
    };

    Regex::new(&format!(
        r"(?ims){preceding_blank}^# BEGIN railway:[^\r\n]*[ \t]*\r?\n.*?^# END railway:[^\r\n]*[ \t]*(?:\r?\n)?"
    ))
    .context("Failed to build Railway SSH config block regex")
}

fn block_matches_service_name(block: &str, service_name: &str) -> bool {
    block_service_metadata_matches(block, service_name)
        || block_legacy_marker_matches(block, service_name)
}

fn block_service_metadata_matches(block: &str, service_name: &str) -> bool {
    let service_name = marker_name(service_name);

    block.lines().any(|line| {
        line.strip_prefix("# Railway service:")
            .is_some_and(|line_service_name| {
                line_service_name.trim().eq_ignore_ascii_case(&service_name)
            })
    })
}

fn block_legacy_marker_matches(block: &str, service_name: &str) -> bool {
    let service_name = marker_name(service_name);

    block.lines().any(|line| {
        line.strip_prefix("# BEGIN railway:")
            .is_some_and(|marker| marker.trim().eq_ignore_ascii_case(&service_name))
    })
}

fn target_marker(project_id: &str, environment_id: &str, service_id: &str) -> String {
    format!(
        "{}:{}:{}",
        marker_name(project_id),
        marker_name(environment_id),
        marker_name(service_id)
    )
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
    fn renders_config_block_with_minimal_markers() {
        let block = render_config_block(
            "api",
            "project-id:environment-id:service-id",
            "railway-api",
            "instance-id",
            None,
        );

        assert_eq!(
            block,
            "\
# BEGIN railway:project-id:environment-id:service-id
# Railway service: api
Host railway-api
    HostName ssh.railway.com
    User instance-id
    ServerAliveInterval 30
    ServerAliveCountMax 3
# END railway:project-id:environment-id:service-id
"
        );
    }

    #[test]
    fn upserts_block_idempotently_and_sets_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ssh/config");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "\
Host existing
    HostName example.com

# BEGIN railway:API
Host railway-api
    User old-id
# END railway:API
",
        )
        .unwrap();

        let new_block = render_config_block(
            "api",
            "project-id:environment-id:service-id",
            "railway-api",
            "new-id",
            Some(Path::new("/Users/me/My Keys/id_ed25519")),
        );

        upsert_config_block(
            &path,
            "project-id:environment-id:service-id",
            "api",
            &new_block,
        )
        .unwrap();
        upsert_config_block(
            &path,
            "project-id:environment-id:service-id",
            "api",
            &new_block,
        )
        .unwrap();

        let contents = fs::read_to_string(&path).unwrap();

        assert_eq!(contents.matches("# BEGIN railway:").count(), 1);
        assert!(contents.starts_with("Host existing\n    HostName example.com\n\n"));
        assert!(contents.contains("# BEGIN railway:project-id:environment-id:service-id\n"));
        assert!(contents.contains("# Railway service: api\n"));
        assert!(contents.contains("Host railway-api\n"));
        assert!(contents.contains("    HostName ssh.railway.com\n"));
        assert!(contents.contains("    User new-id\n"));
        assert!(contents.contains("    IdentityFile \"/Users/me/My Keys/id_ed25519\"\n"));
        assert!(contents.contains("# END railway:project-id:environment-id:service-id\n"));
        assert!(!contents.contains("written"));
        assert!(!contents.contains("If you rename"));
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

    #[test]
    fn upserts_same_service_name_for_different_targets_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ssh/config");

        let prod_block = render_config_block(
            "api",
            "project-id:production-id:service-id",
            "railway-api",
            "prod-instance-id",
            None,
        );
        let staging_block = render_config_block(
            "api",
            "project-id:staging-id:service-id",
            "railway-api-staging",
            "staging-instance-id",
            None,
        );
        let updated_prod_block = render_config_block(
            "api",
            "project-id:production-id:service-id",
            "railway-api-prod",
            "updated-prod-instance-id",
            None,
        );

        upsert_config_block(
            &path,
            "project-id:production-id:service-id",
            "api",
            &prod_block,
        )
        .unwrap();
        upsert_config_block(
            &path,
            "project-id:staging-id:service-id",
            "api",
            &staging_block,
        )
        .unwrap();
        upsert_config_block(
            &path,
            "project-id:production-id:service-id",
            "api",
            &updated_prod_block,
        )
        .unwrap();

        let contents = fs::read_to_string(&path).unwrap();

        assert_eq!(contents.matches("# BEGIN railway:").count(), 2);
        assert!(contents.contains("Host railway-api-prod\n"));
        assert!(contents.contains("    User updated-prod-instance-id\n"));
        assert!(contents.contains("Host railway-api-staging\n"));
        assert!(contents.contains("    User staging-instance-id\n"));
        assert!(!contents.contains("    User prod-instance-id\n"));
    }

    #[test]
    fn remove_target_keeps_same_service_name_for_other_targets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ssh/config");

        let prod_block = render_config_block(
            "api",
            "project-id:production-id:service-id",
            "railway-api",
            "prod-instance-id",
            None,
        );
        let staging_block = render_config_block(
            "api",
            "project-id:staging-id:service-id",
            "railway-api-staging",
            "staging-instance-id",
            None,
        );

        upsert_config_block(
            &path,
            "project-id:production-id:service-id",
            "api",
            &prod_block,
        )
        .unwrap();
        upsert_config_block(
            &path,
            "project-id:staging-id:service-id",
            "api",
            &staging_block,
        )
        .unwrap();

        assert!(
            remove_config_block_for_target(&path, "project-id:production-id:service-id", "api")
                .unwrap()
        );

        let contents = fs::read_to_string(&path).unwrap();

        assert_eq!(contents.matches("# BEGIN railway:").count(), 1);
        assert!(!contents.contains("prod-instance-id"));
        assert!(contents.contains("staging-instance-id"));
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

# BEGIN railway:API
Host old
# END railway:API

# BEGIN railway:project-id:environment-id:service-id
# Railway service: API
Host new
# END railway:project-id:environment-id:service-id
Host later
",
        )
        .unwrap();

        command(Args {
            command: Some(Commands::Remove),
            target: TargetArgs {
                service: Some("api".to_string()),
                path: path.clone(),
                ..TargetArgs::default()
            },
            alias: None,
            identity_file: None,
            dry_run: false,
        })
        .await
        .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "Host existing\nHost later\n"
        );

        command(Args {
            command: Some(Commands::Remove),
            target: TargetArgs {
                service: Some("API".to_string()),
                path,
                ..TargetArgs::default()
            },
            alias: None,
            identity_file: None,
            dry_run: false,
        })
        .await
        .unwrap();
    }
}
