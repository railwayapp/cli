//! Migrate Config as Code (`railway.json` / `railway.toml`) into
//! `.railway/railway.ts`. CaC → graph/DSL translation lives in the CLI only.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use colored::Colorize;
use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::{
    client::{GQLClient, post_graphql, post_graphql_raw},
    config::Configs,
    gql::mutations::{self, ServiceInstanceUpdate},
    iac::{EvalContext, evaluate_file_with_context},
    util::cac_deprecation::{find_all_cac_files, find_cac_file},
    util::prompt::prompt_confirm_with_default,
};

use super::*;

#[derive(Parser)]
pub struct MigrateArgs {
    #[clap(subcommand)]
    command: Option<MigrateCommand>,

    /// Write `.railway/railway.ts` (default is dry-run). This only touches the
    /// filesystem. Services keep reading Config as Code until you run
    /// `railway config migrate cutover`.
    #[clap(long)]
    apply: bool,

    /// Overwrite an existing `.railway/railway.ts`.
    #[clap(long)]
    force: bool,

    /// Print the full generated authoring file instead of a summary.
    #[clap(long)]
    show: bool,

    /// Delete discovered `railway.json` / `railway.toml` from disk. Push the
    /// deletions only after cutover; the platform reads them until then.
    #[clap(long)]
    delete_files: bool,

    /// Migrate only the service with this name. With a single Config as Code
    /// file it instead overrides the emitted service name.
    #[clap(long)]
    service: Option<String>,

    /// Authoring language to emit: `ts` (default), `py`, or `go`.
    #[clap(long, default_value = "ts")]
    lang: String,
}

#[derive(Parser)]
enum MigrateCommand {
    /// Switch services off Config as Code so IaC can manage them. Saves a
    /// snapshot of the current config-file paths so the switch is reversible.
    Cutover(CutoverArgs),

    /// Restore the Config as Code paths saved by the last cutover.
    Undo(UndoArgs),

    /// Show migration progress: which services still read Config as Code.
    Status,
}

#[derive(Parser)]
struct CutoverArgs {
    /// Cut over only this service.
    #[clap(long)]
    service: Option<String>,

    /// Skip the confirmation prompt.
    #[clap(long)]
    yes: bool,
}

#[derive(Parser)]
struct UndoArgs {
    /// Skip the confirmation prompt.
    #[clap(long)]
    yes: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CacFile {
    #[serde(default)]
    build: CacBuild,
    #[serde(default)]
    deploy: CacDeploy,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CacBuild {
    builder: Option<String>,
    build_command: Option<String>,
    dockerfile_path: Option<String>,
    watch_patterns: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CacDeploy {
    start_command: Option<String>,
    pre_deploy_command: Option<JsonValue>,
    healthcheck_path: Option<String>,
    healthcheck_timeout: Option<i64>,
    num_replicas: Option<i64>,
    region: Option<String>,
    multi_region_config: Option<JsonValue>,
    cron_schedule: Option<String>,
}

struct CacService {
    name: String,
    path: PathBuf,
    cac: CacFile,
}

pub async fn migrate_config(args: MigrateArgs) -> Result<()> {
    match &args.command {
        Some(MigrateCommand::Cutover(cutover_args)) => return cutover(cutover_args).await,
        Some(MigrateCommand::Undo(undo_args)) => return undo(undo_args).await,
        Some(MigrateCommand::Status) => return migrate_status().await,
        None => generate(args).await,
    }
}

/// Discover Config as Code files and write `.railway/railway.ts`. Filesystem
/// only — the remote cutover is a separate, reversible step so a generate can
/// never leave a service with neither config.
async fn generate(args: MigrateArgs) -> Result<()> {
    if !matches!(args.lang.as_str(), "ts" | "py" | "go") {
        bail!("--lang must be one of: ts, py, go");
    }
    if args.delete_files && !args.apply {
        bail!("--delete-files requires --apply.");
    }

    let cwd = std::env::current_dir().context("Unable to get current directory")?;
    let services = discover_cac_services(&cwd, args.service.as_deref()).await?;
    let project_name = project_name_for_emit(&cwd, &services).await;
    let named_partial = services.len() == 1;

    let railway_dir = cwd.join(".railway");
    let ext = match args.lang.as_str() {
        "py" => "py",
        "go" => "go",
        _ => "ts",
    };
    let railway_file = railway_dir.join(format!("railway.{ext}"));
    let emitted = match args.lang.as_str() {
        "py" => emit_railway_py(&project_name, &services, named_partial),
        "go" => emit_railway_go(&project_name, &services, named_partial),
        _ => emit_railway_ts(&project_name, &services, named_partial),
    };

    // `--show` prints just the file so `migrate --show > out.ts` works.
    if args.show && !args.apply {
        println!("{emitted}");
        return Ok(());
    }

    let environment = linked_environment_name().await;
    print_migration_preview(&cwd, &services, &project_name, environment.as_deref(), ext);

    let interactive = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();

    // --apply writes unconditionally. Bare `migrate` in a terminal asks;
    // piped/non-interactive stays a pure dry run.
    if !args.apply {
        if !interactive {
            eprintln!("\n{} Nothing changed. This was a dry run.", "Note:".dimmed());
            eprintln!("\n{}", "Next".bold());
            eprintln!(
                "  {} {}   write the file",
                "•".dimmed(),
                "railway config migrate --apply".cyan()
            );
            return Ok(());
        }
        eprintln!("\n{} Nothing changed yet. This was a dry run.", "Note:".dimmed());
        eprintln!();
        let write =
            prompt_confirm_with_default(&format!("Write .railway/railway.{ext} now?"), false)?;
        if !write {
            eprintln!(
                "\n{} When you're ready: {}",
                "Note:".dimmed(),
                "railway config migrate --apply".cyan()
            );
            return Ok(());
        }
    }

    if railway_file.exists() && !args.force {
        bail!(
            "{} already exists. Pass --force to overwrite, or merge the dry-run output manually.",
            railway_file.display()
        );
    }

    fs::create_dir_all(&railway_dir)?;
    fs::write(&railway_file, &emitted)
        .with_context(|| format!("Failed to write {}", railway_file.display()))?;
    eprintln!(
        "\n{} {}",
        "Wrote".green().bold(),
        display_rel(&cwd, &railway_file).cyan()
    );
    match args.lang.as_str() {
        "go" => {
            let gomod = railway_dir.join("go.mod");
            if !gomod.exists() {
                fs::write(
                    &gomod,
                    "module railway-config\n\ngo 1.22\n\nrequire github.com/railwayapp/railway-go-sdk v0.2.0\n",
                )?;
            }
        }
        "py" => {
            let req = railway_dir.join("requirements.txt");
            if !req.exists() {
                fs::write(&req, "railway-sdk>=0.2.0\n")?;
            }
        }
        _ => {}
    }

    if args.delete_files {
        for service in &services {
            fs::remove_file(&service.path)
                .with_context(|| format!("Failed to delete {}", service.path.display()))?;
            eprintln!(
                "{} {}",
                "Deleted".green().bold(),
                display_rel(&cwd, &service.path).cyan()
            );
        }
        eprintln!(
            "  {} Push these deletions only after cutover — the platform reads them until then.",
            "!".yellow().bold()
        );
    }

    // Offer the next step. Cutover redeploys services, so we only reach it
    // behind an explicit human confirmation in a terminal.
    if interactive {
        eprintln!(
            "\n{} switches these services off Config as Code and redeploys them.",
            "Cutover".bold()
        );
        eprintln!(
            "  {} Environments that inherit this one pick it up too. A snapshot is saved for undo.",
            "!".yellow().bold()
        );
        let go = prompt_confirm_with_default("Switch them off now? (cutover)", false)?;
        if go {
            let declared: HashSet<String> =
                services.iter().map(|service| service.name.clone()).collect();
            return run_cutover(declared, None, true).await;
        }
        eprintln!(
            "\n{} File written. When you're ready: {}",
            "Note:".dimmed(),
            "railway config migrate cutover".cyan()
        );
        return Ok(());
    }

    eprintln!("\n{}", "Next".bold());
    eprintln!(
        "  {} {}   switch these services off Config as Code (saves a snapshot)",
        "•".dimmed(),
        "railway config migrate cutover".cyan()
    );
    eprintln!(
        "  {} {}             preview the IaC changes",
        "•".dimmed(),
        "railway config plan".cyan()
    );
    eprintln!(
        "  {} {}            hand management to IaC",
        "•".dimmed(),
        "railway config apply".cyan()
    );
    eprintln!(
        "\n{} Cutover clears each service's Config File path and redeploys it, so\n  run it right before {}. Config as Code keeps working until then.",
        "Note:".yellow().bold(),
        "railway config plan".cyan()
    );
    Ok(())
}

async fn linked_environment_name() -> Option<String> {
    let configs = Configs::new().ok()?;
    let linked = configs.get_linked_project().await.ok()?;
    linked.environment_name
}

fn pluralize(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Short, scannable preview of what migration found and generated. The full
/// file is written on apply, or printed with `--show`. Columns are padded on
/// the plain strings before coloring so ANSI codes don't break alignment.
fn print_migration_preview(
    cwd: &Path,
    services: &[CacService],
    project_name: &str,
    environment: Option<&str>,
    ext: &str,
) {
    eprintln!("\n{}", "Railway configuration".bold());
    eprintln!("  {}      {}", "Project".dimmed(), project_name.cyan());
    if let Some(env) = environment {
        eprintln!("  {}  {}", "Environment".dimmed(), env.cyan());
    }

    let count = services.len();
    eprintln!(
        "\n{} {} managed by Config as Code",
        "Found".bold(),
        pluralize(count, "service")
    );
    let path_width = services
        .iter()
        .map(|service| display_rel(cwd, &service.path).chars().count())
        .max()
        .unwrap_or(0);
    for service in services {
        let padded = format!(
            "{:<width$}",
            display_rel(cwd, &service.path),
            width = path_width
        );
        eprintln!("  {}  {} {}", padded.dimmed(), "→".dimmed(), service.name.cyan());
    }

    let scope = if count > 1 {
        format!("merged, {count} services")
    } else {
        format!("service {}", services[0].name)
    };
    eprintln!(
        "\n{} {} ({})",
        "Generated".bold(),
        format!(".railway/railway.{ext}").cyan(),
        scope.dimmed()
    );
    let name_width = services
        .iter()
        .map(|service| service.name.chars().count())
        .max()
        .unwrap_or(0);
    for service in services {
        let (builder, carried) = summarize_cac(&service.cac);
        let name_padded = format!("{:<width$}", service.name, width = name_width);
        let builder_padded = format!("{builder:<10}");
        eprintln!(
            "  {}  {}  {}",
            name_padded.cyan(),
            builder_padded.magenta(),
            carried.dimmed()
        );
    }

    eprintln!(
        "\n  {} Run {} for the full file.",
        "→".dimmed(),
        "railway config migrate --show".cyan()
    );
}

/// A one-line summary of what a Config as Code file carries into IaC.
fn summarize_cac(cac: &CacFile) -> (String, String) {
    let builder = cac
        .build
        .builder
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "—".to_string());

    let mut carried = Vec::new();
    if cac.build.build_command.is_some() {
        carried.push("build".to_string());
    }
    if cac.build.dockerfile_path.is_some() {
        carried.push("dockerfile".to_string());
    }
    if cac.deploy.start_command.is_some() {
        carried.push("start".to_string());
    }
    if cac.deploy.healthcheck_path.is_some() {
        carried.push("healthcheck".to_string());
    }
    if let Some(timeout) = cac.deploy.healthcheck_timeout {
        carried.push(format!("healthcheckTimeout {timeout}"));
    }
    if cac.deploy.pre_deploy_command.is_some() {
        carried.push("preDeploy".to_string());
    }
    if let Some(replicas) = cac.deploy.num_replicas {
        carried.push(format!("replicas {replicas}"));
    }
    if cac.deploy.multi_region_config.is_some() {
        carried.push("regions".to_string());
    }
    if cac.deploy.cron_schedule.is_some() {
        carried.push("cron".to_string());
    }
    if cac.deploy.region.is_some() {
        carried.push("region".to_string());
    }

    let summary = if carried.is_empty() {
        "no overrides".to_string()
    } else {
        carried.join(" + ")
    };
    (builder, summary)
}

async fn discover_cac_services(
    cwd: &Path,
    service_filter: Option<&str>,
) -> Result<Vec<CacService>> {
    let root = git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut files = find_all_cac_files(&root);
    if files.is_empty() {
        if let Some(one) = find_cac_file(cwd) {
            files.push(one);
        }
    }
    if files.is_empty() {
        bail!("No railway.json or railway.toml found in this repository.");
    }

    let env_index = environment_cac_index(&root).await.unwrap_or_default();
    let mut claimed = HashSet::new();
    let mut services = Vec::new();

    for (rel, meta) in &env_index {
        let path = root.join(rel);
        if !path.is_file() {
            eprintln!(
                "{} {} is set as the Railway Config File for {} but was not found on disk.",
                "Warning:".yellow().bold(),
                rel.cyan(),
                meta.name.cyan()
            );
            continue;
        }
        let cac = parse_cac_file(&path)?;
        claimed.insert(canonicalize_or_clone(&path));
        services.push(CacService {
            name: meta.name.clone(),
            path,
            cac,
        });
    }

    for path in files {
        if claimed.contains(&canonicalize_or_clone(&path)) {
            continue;
        }
        let name = guess_service_name(cwd, &path);
        let cac = parse_cac_file(&path)?;
        services.push(CacService {
            name,
            path,
            cac,
        });
    }

    apply_service_filter(&mut services, service_filter)?;

    services.sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));

    let mut seen = HashSet::new();
    for service in &services {
        if !seen.insert(service.name.clone()) {
            bail!(
                "Two Config as Code files map to the service name {}. Rename one of the directories or remove one of the files.",
                service.name
            );
        }
    }

    Ok(services)
}

fn apply_service_filter(services: &mut Vec<CacService>, filter: Option<&str>) -> Result<()> {
    let Some(filter) = filter else {
        return Ok(());
    };
    if services.iter().any(|service| service.name == filter) {
        services.retain(|service| service.name == filter);
    } else if services.len() == 1 {
        // Single-file repos historically used --service to override the
        // guessed name; keep that working.
        services[0].name = filter.to_string();
    } else {
        bail!("No Config as Code file found for service {filter}.");
    }
    Ok(())
}

fn canonicalize_or_clone(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn git_root(start: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn display_rel(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .or_else(|_| path.strip_prefix(git_root(cwd).unwrap_or_else(|| cwd.to_path_buf())))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

struct EnvCacMeta {
    name: String,
}

async fn environment_cac_index(root: &Path) -> Result<BTreeMap<String, EnvCacMeta>> {
    let configs = Configs::new()?;
    let linked = configs.get_linked_project().await?;
    let environment_id = linked
        .environment
        .clone()
        .context("No linked environment")?;
    let client = GQLClient::new_authorized(&configs)?;
    let endpoint = configs.get_backboard();

    #[derive(Deserialize)]
    struct EnvQuery {
        environment: EnvNode,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct EnvNode {
        config: JsonValue,
    }
    #[derive(Deserialize)]
    struct ProjectQuery {
        project: Option<ProjectNode>,
    }
    #[derive(Deserialize)]
    struct ProjectNode {
        services: Option<ServiceConnection>,
    }
    #[derive(Deserialize)]
    struct ServiceConnection {
        edges: Vec<ServiceEdge>,
    }
    #[derive(Deserialize)]
    struct ServiceEdge {
        node: ServiceNode,
    }
    #[derive(Deserialize)]
    struct ServiceNode {
        id: String,
        name: Option<String>,
    }

    let env = post_graphql_raw::<EnvQuery, _>(
        &client,
        &endpoint,
        "query IacMigrateEnv($id: String!) { environment(id: $id) { config } }",
        json!({ "id": environment_id }),
    )
    .await?;
    let names = post_graphql_raw::<ProjectQuery, _>(
        &client,
        &endpoint,
        "query IacMigrateServices($id: String!) { project(id: $id) { services(first: 1000) { edges { node { id name } } } } }",
        json!({ "id": linked.project }),
    )
    .await
    .ok()
    .and_then(|data| data.project)
    .and_then(|project| project.services)
    .map(|connection| {
        connection
            .edges
            .into_iter()
            .map(|edge| (edge.node.id, edge.node.name.unwrap_or_default()))
            .collect::<BTreeMap<_, _>>()
    })
    .unwrap_or_default();

    let mut index = BTreeMap::new();
    let Some(services) = env
        .environment
        .config
        .get("services")
        .and_then(JsonValue::as_object)
    else {
        return Ok(index);
    };
    for (id, service) in services {
        let Some(config_file) = service.get("configFile").and_then(JsonValue::as_str) else {
            continue;
        };
        if !is_cac_config_file(config_file) {
            continue;
        }
        let Some(rel) = normalize_config_file_path(config_file) else {
            eprintln!(
                "{} Railway Config File path {} for service {} escapes the repository; skipping.",
                "Warning:".yellow().bold(),
                config_file.cyan(),
                names.get(id).cloned().unwrap_or_else(|| id.clone()).cyan()
            );
            continue;
        };
        let rel = rel.as_str();
        let name = names
            .get(id)
            .cloned()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| guess_service_name(root, Path::new(rel)));
        index.insert(
            rel.to_string(),
            EnvCacMeta { name },
        );
    }
    Ok(index)
}

/// The platform stores `configFile` as repo-root-relative, but users commonly
/// write a leading `/` (or `./`). Strip those so `root.join(rel)` doesn't
/// discard `root`. Returns `None` for paths with `..` segments.
fn normalize_config_file_path(config_file: &str) -> Option<String> {
    let mut rel = config_file.trim();
    loop {
        let next = rel.trim_start_matches('/').trim_start_matches("./");
        #[cfg(windows)]
        let next = next.trim_start_matches('\\').trim_start_matches(".\\");
        if next.len() == rel.len() {
            break;
        }
        rel = next;
    }
    if rel.split(['/', '\\']).any(|segment| segment == "..") {
        return None;
    }
    Some(rel.to_string())
}

fn is_cac_config_file(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    name == "railway.json" || name == "railway.toml"
}

async fn project_name_for_emit(cwd: &Path, services: &[CacService]) -> String {
    if let Ok(configs) = Configs::new() {
        if let Ok(linked) = configs.get_linked_project().await {
            if let Some(name) = linked.name.filter(|name| !name.is_empty()) {
                return name;
            }
        }
    }
    git_root(cwd)
        .and_then(|root| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|name| !name.is_empty())
        .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| {
            services
                .first()
                .map(|service| service.name.clone())
                .unwrap_or_else(|| "app".to_string())
        })
}

fn parse_cac_file(path: &Path) -> Result<CacFile> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "toml" => toml::from_str(&contents)
            .with_context(|| format!("Failed to parse TOML {}", path.display())),
        "json" => serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse JSON {}", path.display())),
        other => bail!("Unsupported Config as Code extension: .{other}"),
    }
}

fn guess_service_name(cwd: &Path, cac_path: &Path) -> String {
    cac_path
        .parent()
        .and_then(|p| {
            if p == cwd {
                cwd.file_name().map(|n| n.to_string_lossy().into_owned())
            } else {
                p.file_name().map(|n| n.to_string_lossy().into_owned())
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "web".to_string())
}

fn emit_railway_py(project_name: &str, services: &[CacService], named_partial: bool) -> String {
    let mut stmts = Vec::new();
    let mut idents = Vec::new();
    for service in services {
        let ident = service_ident(&service.name, &idents);
        let mut kwargs = Vec::new();
        if let Some(cmd) = &service.cac.build.build_command {
            kwargs.push(format!("        build={}", js_string(cmd)));
        }
        if let Some(cmd) = &service.cac.deploy.start_command {
            kwargs.push(format!("        start={}", js_string(cmd)));
        }
        if let Some(path) = &service.cac.deploy.healthcheck_path {
            kwargs.push(format!("        healthcheck={}", js_string(path)));
        }
        stmts.push(if kwargs.is_empty() {
            format!("    {ident} = service({})", js_string(&service.name))
        } else {
            format!(
                "    {ident} = service(\n        {},\n{},\n    )",
                js_string(&service.name),
                kwargs.join(",\n")
            )
        });
        idents.push(ident);
    }
    let partial = if named_partial {
        format!(
            "\n# This repository manages only its own resources in the environment. Other\n# repositories export their own partial name.\n# See https://docs.railway.com/infrastructure-as-code#multi-repo-projects\nPARTIAL = {}\n",
            js_string(&services[0].name)
        )
    } else {
        String::new()
    };
    format!(
        r#"from railway_sdk import define_railway, project, service
{partial}
@define_railway
def main(ctx=None):
{stmts}
    return project({project}, resources=[{resources}])
"#,
        stmts = stmts.join("\n"),
        project = js_string(project_name),
        resources = idents.join(", "),
    )
}

fn emit_railway_go(project_name: &str, services: &[CacService], named_partial: bool) -> String {
    let mut stmts = Vec::new();
    let mut idents = Vec::new();
    for service in services {
        let ident = service_ident(&service.name, &idents);
        let mut fields = Vec::new();
        if let Some(cmd) = &service.cac.build.build_command {
            fields.push(format!("\t\t\"build\": {},", js_string(cmd)));
        }
        if let Some(cmd) = &service.cac.deploy.start_command {
            fields.push(format!("\t\t\"start\": {},", js_string(cmd)));
        }
        if let Some(path) = &service.cac.deploy.healthcheck_path {
            fields.push(format!("\t\t\"healthcheck\": {},", js_string(path)));
        }
        let config_block = if fields.is_empty() {
            "nil".to_string()
        } else {
            format!("railway.ServiceConfig{{\n{}\n\t}}", fields.join("\n"))
        };
        stmts.push(format!(
            "\t{ident} := railway.ServiceNamed({}, {config_block})",
            js_string(&service.name)
        ));
        idents.push(ident);
    }
    let partial = if named_partial {
        format!(
            "\n// This repository manages only its own resources in the environment. Other\n// repositories export their own partial name.\n// See https://docs.railway.com/infrastructure-as-code#multi-repo-projects\nconst Partial = {}\n",
            js_string(&services[0].name)
        )
    } else {
        String::new()
    };
    let resources = idents.join(", ");
    format!(
        r#"package main

import "github.com/railwayapp/railway-go-sdk"
{partial}
func Railway(ctx railway.Context) railway.Project {{
	ctx = railway.NewContext(ctx)
{stmts}
	return railway.ProjectNamed({project}, []any{{{resources}}})
}}
"#,
        stmts = stmts.join("\n"),
        project = js_string(project_name),
    )
}

fn emit_service_fields(cac: &CacFile) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    if let Some(cmd) = &cac.build.build_command {
        fields.push(format!("    build: {},", js_string(cmd)));
    }
    if let Some(cmd) = &cac.deploy.start_command {
        fields.push(format!("    start: {},", js_string(cmd)));
    }
    if let Some(path) = &cac.deploy.healthcheck_path {
        fields.push(format!("    healthcheck: {},", js_string(path)));
    }
    if let Some(timeout) = cac.deploy.healthcheck_timeout {
        fields.push(format!("    healthcheckTimeout: {timeout},"));
    }
    if let Some(replicas) = cac.deploy.num_replicas {
        fields.push(format!("    replicas: {replicas},"));
    }
    if let Some(regions) = &cac.deploy.multi_region_config {
        fields.push(format!("    replicas: {},", json_to_ts(regions)));
    } else if let Some(region) = &cac.deploy.region {
        fields.push(format!("    replicas: {{ {}: 1 }},", js_string(region)));
    }
    if let Some(pre) = &cac.deploy.pre_deploy_command {
        let rendered = match pre {
            JsonValue::Array(items) if items.len() == 1 && items[0].is_string() => {
                js_string(items[0].as_str().unwrap_or_default())
            }
            JsonValue::String(cmd) => js_string(cmd),
            other => json_to_ts(other),
        };
        fields.push(format!("    preDeploy: {rendered},"));
    }
    if let Some(dockerfile) = &cac.build.dockerfile_path {
        fields.push(format!(
            "    // dockerfilePath from CaC: {}",
            js_string(dockerfile)
        ));
    }
    if let Some(builder) = &cac.build.builder {
        fields.push(format!("    // builder from CaC: {}", js_string(builder)));
    }
    if let Some(cron) = &cac.deploy.cron_schedule {
        fields.push(format!("    // cronSchedule from CaC: {}", js_string(cron)));
    }
    if let Some(watch) = &cac.build.watch_patterns {
        let arr = watch
            .iter()
            .map(|p| js_string(p))
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(format!("    // watchPatterns from CaC: [{arr}]"));
    }
    fields
}

fn emit_railway_ts(project_name: &str, services: &[CacService], named_partial: bool) -> String {
    let mut stmts = Vec::new();
    let mut idents = Vec::new();
    for service in services {
        let ident = service_ident(&service.name, &idents);
        let fields = emit_service_fields(&service.cac);
        stmts.push(if fields.is_empty() {
            format!("  const {ident} = service({});", js_string(&service.name))
        } else {
            format!(
                "  const {ident} = service({}, {{\n{}\n  }});",
                js_string(&service.name),
                fields.join("\n")
            )
        });
        idents.push(ident);
    }
    let partial = if named_partial {
        format!(
            "\n// This repository manages only its own resources in the environment. Other\n// repositories export their own partial name.\n// See https://docs.railway.com/infrastructure-as-code#multi-repo-projects\nexport const partial = {};\n",
            js_string(&services[0].name)
        )
    } else {
        String::new()
    };
    format!(
        r#"import {{ defineRailway, project, service }} from "railway/iac";
{partial}
export default defineRailway(() => {{
{body}
  return project({project}, {{
    resources: [{resources}],
  }});
}});
"#,
        body = stmts.join("\n"),
        project = js_string(project_name),
        resources = idents.join(", "),
    )
}

fn service_ident(name: &str, taken: &[String]) -> String {
    let mut ident: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if ident.is_empty() || ident.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        ident = format!("service_{ident}");
    }
    if matches!(
        ident.as_str(),
        "service" | "project" | "default" | "package" | "func" | "main"
    ) {
        ident = format!("{ident}_service");
    }
    let base = ident.clone();
    let mut n = 2;
    while taken.contains(&ident) {
        ident = format!("{base}_{n}");
        n += 1;
    }
    ident
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("{:?}", value))
}

fn json_to_ts(value: &JsonValue) -> String {
    match value {
        JsonValue::Object(map) => {
            let fields = map
                .iter()
                .map(|(k, v)| format!("{}: {}", js_string(k), json_to_ts(v)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {fields} }}")
        }
        JsonValue::Array(items) => {
            let inner = items.iter().map(json_to_ts).collect::<Vec<_>>().join(", ");
            format!("[{inner}]")
        }
        JsonValue::String(s) => js_string(s),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Null => "null".to_string(),
    }
}

// ===== Cutover / undo / status =====
//
// Generate only writes files. Switching services off Config as Code is a
// separate, explicit, reversible step: it clears each service's config-file
// path (which redeploys it) so IaC can manage it, after saving a snapshot of
// the previous paths so the switch can be undone.

const SNAPSHOT_FILE: &str = ".cac-migration.json";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CutoverSnapshot {
    version: u32,
    environment_id: String,
    environment_name: Option<String>,
    created_at: String,
    services: Vec<SnapshotService>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SnapshotService {
    service_id: String,
    service_name: String,
    railway_config_file: String,
}

/// A service in the linked environment that still reads Config as Code.
struct CacInstance {
    service_id: String,
    service_name: String,
    config_file: String,
}

fn snapshot_path(cwd: &Path) -> PathBuf {
    cwd.join(".railway").join(SNAPSHOT_FILE)
}

fn write_snapshot(cwd: &Path, snapshot: &CutoverSnapshot) -> Result<()> {
    fs::create_dir_all(cwd.join(".railway"))?;
    let path = snapshot_path(cwd);
    let body = serde_json::to_string_pretty(snapshot)?;
    fs::write(&path, body).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

fn read_snapshot(cwd: &Path) -> Result<Option<CutoverSnapshot>> {
    let path = snapshot_path(cwd);
    match fs::read_to_string(&path) {
        Ok(contents) => Ok(Some(
            serde_json::from_str(&contents)
                .with_context(|| format!("Failed to parse {}", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn find_authoring_file(cwd: &Path) -> Option<PathBuf> {
    const NAMES: &[&str] = &["railway.ts", "railway.py", "railway.go"];
    for dir in [cwd.to_path_buf(), cwd.join(".railway")] {
        for name in NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Evaluate the authoring file to learn which services (and databases) it
/// declares, so cutover never touches a service that hasn't been migrated yet.
async fn declared_services(cwd: &Path) -> Result<HashSet<String>> {
    let file = find_authoring_file(cwd).context(
        "No .railway/railway.{ts,py,go} found. Run `railway config migrate --apply` first.",
    )?;
    let ctx = match Configs::new() {
        Ok(configs) => match configs.get_linked_project().await {
            Ok(linked) => EvalContext::from_linked_project(&linked, "migrate"),
            Err(_) => EvalContext::default(),
        },
        Err(_) => EvalContext::default(),
    };
    let evaluated = evaluate_file_with_context(&file, &ctx)?;
    Ok(evaluated
        .graph
        .resources
        .iter()
        .filter(|resource| {
            matches!(
                resource.get("type").and_then(JsonValue::as_str),
                Some("service") | Some("database")
            )
        })
        .filter_map(|resource| resource.get("name").and_then(JsonValue::as_str))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

async fn fetch_cac_instances(configs: &Configs, environment_id: &str) -> Result<Vec<CacInstance>> {
    #[derive(Deserialize)]
    struct EnvQuery {
        environment: EnvNode,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct EnvNode {
        service_instances: Conn,
    }
    #[derive(Deserialize)]
    struct Conn {
        edges: Vec<Edge>,
    }
    #[derive(Deserialize)]
    struct Edge {
        node: Node,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Node {
        service_id: String,
        service_name: String,
        railway_config_file: Option<String>,
    }

    let client = GQLClient::new_authorized(configs)?;
    let edges = post_graphql_raw::<EnvQuery, _>(
        &client,
        &configs.get_backboard(),
        "query IacCutoverInstances($id: String!) { environment(id: $id) { serviceInstances(first: 1000) { edges { node { serviceId serviceName railwayConfigFile } } } } }",
        json!({ "id": environment_id }),
    )
    .await?
    .environment
    .service_instances
    .edges;

    Ok(edges
        .into_iter()
        .filter_map(|edge| {
            let config_file = edge.node.railway_config_file.unwrap_or_default();
            if config_file.trim().is_empty() || !is_cac_config_file(config_file.trim()) {
                return None;
            }
            Some(CacInstance {
                service_id: edge.node.service_id,
                service_name: edge.node.service_name,
                config_file,
            })
        })
        .collect())
}

async fn set_config_file(
    configs: &Configs,
    service_id: &str,
    environment_id: &str,
    value: &str,
) -> Result<()> {
    let client = GQLClient::new_authorized(configs)?;
    let vars = mutations::service_instance_update::Variables {
        service_id: service_id.to_string(),
        environment_id: Some(environment_id.to_string()),
        input: mutations::service_instance_update::ServiceInstanceUpdateInput {
            railway_config_file: Some(value.to_string()),
            ..Default::default()
        },
    };
    post_graphql::<ServiceInstanceUpdate, _>(&client, configs.get_backboard(), vars).await?;
    Ok(())
}

async fn cutover(args: &CutoverArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("Unable to get current directory")?;
    let declared = declared_services(&cwd).await?;
    run_cutover(declared, args.service.as_deref(), args.yes).await
}

/// Switch the declared services that still read Config as Code over to IaC.
/// `assume_yes` skips the final confirmation (the caller already confirmed),
/// but the affected services and the warning are always shown first.
async fn run_cutover(
    declared: HashSet<String>,
    service_filter: Option<&str>,
    assume_yes: bool,
) -> Result<()> {
    let cwd = std::env::current_dir().context("Unable to get current directory")?;
    let configs = Configs::new()?;
    let linked = configs
        .get_linked_project()
        .await
        .context("No linked project. Run `railway link` first.")?;
    let environment_id = linked
        .environment
        .clone()
        .context("No linked environment. Run `railway link` first.")?;
    let env_label = linked
        .environment_name
        .clone()
        .unwrap_or_else(|| environment_id.clone());

    let mut targets: Vec<CacInstance> = fetch_cac_instances(&configs, &environment_id)
        .await?
        .into_iter()
        .filter(|instance| declared.contains(&instance.service_name))
        .filter(|instance| service_filter.is_none_or(|name| instance.service_name == name))
        .collect();
    targets.sort_by(|a, b| a.service_name.cmp(&b.service_name));

    if targets.is_empty() {
        if let Some(name) = service_filter {
            bail!(
                "Service {name} is not managed by Config as Code in {env_label}, or is not declared in your IaC file."
            );
        }
        eprintln!(
            "{} No declared services are still on Config as Code in {}.",
            "Nothing to do.".green().bold(),
            env_label.cyan()
        );
        return Ok(());
    }

    eprintln!("{}", "Cutover".bold());
    eprintln!("  {}  {}", "Environment".dimmed(), env_label.cyan());
    eprintln!();
    eprintln!(
        "This switches {} service(s) off Config as Code:",
        targets.len().to_string().cyan()
    );
    for target in &targets {
        eprintln!(
            "  {} {}  clears {}",
            "-".red(),
            target.service_name.cyan(),
            target.config_file.dimmed()
        );
    }
    eprintln!();
    eprintln!(
        "{} Each service redeploys as IaC takes over, and environments that\n  inherit {} pick this up too.",
        "!".red().bold(),
        env_label.cyan()
    );
    eprintln!(
        "{} Current paths are saved to {} — undo with {}.",
        "!".yellow().bold(),
        ".railway/.cac-migration.json".cyan(),
        "railway config migrate undo".cyan()
    );
    eprintln!();

    if !assume_yes
        && !prompt_confirm_with_default("Switch these services off Config as Code?", false)?
    {
        eprintln!(
            "\n{} No changes made. When you're ready: {}",
            "Aborted.".yellow().bold(),
            "railway config migrate cutover".cyan()
        );
        return Ok(());
    }

    // Snapshot before mutating so undo always has the previous paths.
    let snapshot = CutoverSnapshot {
        version: 1,
        environment_id: environment_id.clone(),
        environment_name: linked.environment_name.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        services: targets
            .iter()
            .map(|target| SnapshotService {
                service_id: target.service_id.clone(),
                service_name: target.service_name.clone(),
                railway_config_file: target.config_file.clone(),
            })
            .collect(),
    };
    write_snapshot(&cwd, &snapshot)?;

    for target in &targets {
        set_config_file(&configs, &target.service_id, &environment_id, "")
            .await
            .with_context(|| {
                format!(
                    "Failed to switch {} off Config as Code. Run `railway config migrate undo` to restore, or clear it in the dashboard.",
                    target.service_name
                )
            })?;
        eprintln!(
            "{} {} off Config as Code",
            "Switched".green().bold(),
            target.service_name.cyan()
        );
    }

    eprintln!("\n{}", "Next".bold());
    eprintln!(
        "  {} {}   preview the IaC changes",
        "•".dimmed(),
        "railway config plan".cyan()
    );
    eprintln!(
        "  {} {}  hand management to IaC",
        "•".dimmed(),
        "railway config apply".cyan()
    );
    eprintln!(
        "\n{} Changed your mind? {} restores the paths above.",
        "Note:".dimmed(),
        "railway config migrate undo".cyan()
    );
    Ok(())
}

async fn undo(args: &UndoArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("Unable to get current directory")?;
    let snapshot = read_snapshot(&cwd)?
        .context("No cutover snapshot found (.railway/.cac-migration.json). Nothing to undo.")?;
    let configs = Configs::new()?;
    let linked = configs
        .get_linked_project()
        .await
        .context("No linked project. Run `railway link` first.")?;
    let environment_id = linked
        .environment
        .clone()
        .context("No linked environment. Run `railway link` first.")?;
    if environment_id != snapshot.environment_id {
        bail!(
            "The snapshot was taken for environment {}. Link that environment before running undo.",
            snapshot
                .environment_name
                .as_deref()
                .unwrap_or(&snapshot.environment_id)
        );
    }
    if snapshot.services.is_empty() {
        eprintln!(
            "{} The snapshot has no services to restore.",
            "Nothing to do.".green().bold()
        );
        return Ok(());
    }

    eprintln!("{}", "Undo cutover".bold());
    eprintln!(
        "Restores the Config as Code path for {} service(s):",
        snapshot.services.len().to_string().cyan()
    );
    for service in &snapshot.services {
        eprintln!(
            "  {} {}  {}",
            "+".green(),
            service.service_name.cyan(),
            service.railway_config_file.dimmed()
        );
    }
    eprintln!(
        "{} Each service redeploys back onto Config as Code.",
        "!".yellow().bold()
    );
    eprintln!();

    if !args.yes && !prompt_confirm_with_default("Restore these Config as Code paths?", false)? {
        eprintln!("\n{} No changes made.", "Aborted.".yellow().bold());
        return Ok(());
    }

    for service in &snapshot.services {
        set_config_file(
            &configs,
            &service.service_id,
            &environment_id,
            &service.railway_config_file,
        )
        .await
        .with_context(|| format!("Failed to restore Config as Code on {}.", service.service_name))?;
        eprintln!(
            "{} {} → {}",
            "Restored".green().bold(),
            service.service_name.cyan(),
            service.railway_config_file.dimmed()
        );
    }

    // Consume the snapshot so a second undo can't replay a stale state.
    let _ = fs::remove_file(snapshot_path(&cwd));
    eprintln!(
        "\n{} Services are back on Config as Code.",
        "Done.".green().bold()
    );
    Ok(())
}

async fn migrate_status() -> Result<()> {
    let cwd = std::env::current_dir().context("Unable to get current directory")?;
    let configs = Configs::new()?;
    let linked = configs
        .get_linked_project()
        .await
        .context("No linked project. Run `railway link` first.")?;
    let environment_id = linked
        .environment
        .clone()
        .context("No linked environment. Run `railway link` first.")?;
    let env_label = linked
        .environment_name
        .clone()
        .unwrap_or_else(|| environment_id.clone());

    let authoring = find_authoring_file(&cwd);
    let declared = if authoring.is_some() {
        declared_services(&cwd).await.ok()
    } else {
        None
    };
    let instances = fetch_cac_instances(&configs, &environment_id).await?;

    eprintln!("{}", "Migration status".bold());
    eprintln!("  {}  {}", "Environment".dimmed(), env_label.cyan());
    match &authoring {
        Some(file) => eprintln!("  {}     {}", "IaC file".dimmed(), display_rel(&cwd, file).cyan()),
        None => eprintln!(
            "  {}     {}",
            "IaC file".dimmed(),
            "none — run `railway config migrate --apply`".yellow()
        ),
    }
    eprintln!();

    if instances.is_empty() {
        eprintln!(
            "{} No services in {} read Config as Code.",
            "✓".green(),
            env_label.cyan()
        );
    } else {
        eprintln!(
            "Still on Config as Code ({}):",
            instances.len().to_string().cyan()
        );
        for instance in &instances {
            let marker = match &declared {
                Some(set) if set.contains(&instance.service_name) => "declared in IaC".green(),
                Some(_) => "not in IaC file".yellow(),
                None => "".normal(),
            };
            eprintln!(
                "  {} {}  {}  {}",
                "-".red(),
                instance.service_name.cyan(),
                instance.config_file.dimmed(),
                marker
            );
        }
    }

    if let Some(snapshot) = read_snapshot(&cwd)? {
        eprintln!(
            "\n{} A cutover snapshot exists ({} service(s)). Undo with {}.",
            "Note:".dimmed(),
            snapshot.services.len().to_string().cyan(),
            "railway config migrate undo".cyan()
        );
    }

    eprintln!("\n{}", "Next".bold());
    if authoring.is_none() {
        eprintln!(
            "  {} {}   generate the IaC file",
            "•".dimmed(),
            "railway config migrate --apply".cyan()
        );
    } else if !instances.is_empty() {
        eprintln!(
            "  {} {}   switch declared services off Config as Code",
            "•".dimmed(),
            "railway config migrate cutover".cyan()
        );
    } else {
        eprintln!(
            "  {} {}   preview the IaC changes",
            "•".dimmed(),
            "railway config plan".cyan()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, cac: CacFile) -> CacService {
        CacService {
            name: name.to_string(),
            path: PathBuf::from(name),
            cac,
        }
    }

    #[test]
    fn normalizes_config_file_paths_relative_to_root() {
        let root = Path::new("/repo");
        for input in [
            "/frontend/railway.json",
            "./frontend/railway.json",
            "frontend/railway.json",
        ] {
            let rel = normalize_config_file_path(input).unwrap();
            assert_eq!(
                root.join(rel),
                root.join("frontend/railway.json"),
                "{input}"
            );
        }
        for input in ["/railway.toml", "./railway.toml", "railway.toml"] {
            let rel = normalize_config_file_path(input).unwrap();
            assert_eq!(root.join(rel), root.join("railway.toml"), "{input}");
        }
    }

    #[test]
    fn rejects_config_file_paths_escaping_root() {
        assert_eq!(normalize_config_file_path("../x/railway.json"), None);
        assert_eq!(
            normalize_config_file_path("/frontend/../../x/railway.json"),
            None
        );
    }

    #[test]
    fn emits_build_and_start() {
        let cac = CacFile {
            build: CacBuild {
                build_command: Some("pnpm build".into()),
                ..Default::default()
            },
            deploy: CacDeploy {
                start_command: Some("pnpm start".into()),
                healthcheck_path: Some("/health".into()),
                ..Default::default()
            },
        };
        let services = [svc("api", cac)];
        let out = emit_railway_ts("api", &services, true);
        assert!(out.contains("build: \"pnpm build\""));
        assert!(out.contains("start: \"pnpm start\""));
        assert!(out.contains("healthcheck: \"/health\""));
        assert!(out.contains("service(\"api\""));
        assert!(out.contains("export const partial = \"api\""));
        let py = emit_railway_py("api", &services, true);
        assert!(py.contains("from railway_sdk import"));
        assert!(py.contains("PARTIAL = \"api\""));
        let go = emit_railway_go("api", &services, true);
        assert!(go.contains("github.com/railwayapp/railway-go-sdk"));
        assert!(go.contains("railway.ServiceNamed"));
        assert!(go.contains("const Partial = \"api\""));
    }

    #[test]
    fn emits_pre_deploy_as_a_real_field() {
        let cac = CacFile {
            deploy: CacDeploy {
                start_command: Some("node index.js".into()),
                pre_deploy_command: Some(serde_json::json!(["npx prisma migrate deploy"])),
                ..Default::default()
            },
            ..Default::default()
        };
        let services = [svc("api", cac)];
        let out = emit_railway_ts("api", &services, true);
        assert!(out.contains("preDeploy: \"npx prisma migrate deploy\""));
        assert!(!out.contains("// preDeployCommand from CaC"));
    }

    #[test]
    fn merges_multiple_services_without_a_partial() {
        let web = svc(
            "web",
            CacFile {
                build: CacBuild {
                    build_command: Some("pnpm --filter web build".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let api = svc(
            "api",
            CacFile {
                deploy: CacDeploy {
                    start_command: Some("node server.js".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let out = emit_railway_ts("acme", &[web, api], false);
        assert!(out.contains("service(\"web\""));
        assert!(out.contains("service(\"api\""));
        assert!(out.contains("project(\"acme\""));
        assert!(out.contains("resources: [web, api]"));
        assert!(!out.contains("export const partial"));
    }

    #[test]
    fn service_filter_selects_matching_service() {
        let mut services = vec![
            svc("web", CacFile::default()),
            svc("api", CacFile::default()),
        ];
        apply_service_filter(&mut services, Some("api")).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "api");
    }

    #[test]
    fn service_filter_renames_a_single_service() {
        let mut services = vec![svc("guessed-dir", CacFile::default())];
        apply_service_filter(&mut services, Some("backend")).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "backend");
    }

    #[test]
    fn service_filter_errors_when_nothing_matches_multiple() {
        let mut services = vec![
            svc("web", CacFile::default()),
            svc("api", CacFile::default()),
        ];
        let err = apply_service_filter(&mut services, Some("worker")).unwrap_err();
        assert!(err.to_string().contains("worker"));
    }

    #[test]
    fn parses_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("railway.toml");
        fs::write(
            &path,
            r#"
[build]
buildCommand = "cargo build"
[deploy]
startCommand = "./app"
healthcheckPath = "/"
"#,
        )
        .unwrap();
        let cac = parse_cac_file(&path).unwrap();
        assert_eq!(cac.build.build_command.as_deref(), Some("cargo build"));
        assert_eq!(cac.deploy.start_command.as_deref(), Some("./app"));
    }

    #[test]
    fn snapshot_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        assert!(read_snapshot(cwd).unwrap().is_none());
        let snapshot = CutoverSnapshot {
            version: 1,
            environment_id: "env-1".into(),
            environment_name: Some("production".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            services: vec![SnapshotService {
                service_id: "svc-1".into(),
                service_name: "web".into(),
                railway_config_file: "packages/web/railway.json".into(),
            }],
        };
        write_snapshot(cwd, &snapshot).unwrap();
        let restored = read_snapshot(cwd).unwrap().expect("snapshot present");
        assert_eq!(restored.environment_id, "env-1");
        assert_eq!(restored.services.len(), 1);
        assert_eq!(restored.services[0].service_name, "web");
        assert_eq!(
            restored.services[0].railway_config_file,
            "packages/web/railway.json"
        );
    }

    #[test]
    fn finds_authoring_file_in_cwd_and_railway_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        assert!(find_authoring_file(cwd).is_none());
        let railway = cwd.join(".railway");
        fs::create_dir_all(&railway).unwrap();
        fs::write(railway.join("railway.ts"), "export default {}").unwrap();
        assert_eq!(find_authoring_file(cwd), Some(railway.join("railway.ts")));
    }
}
