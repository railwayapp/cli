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
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};

use crate::{
    client::{GQLClient, post_graphql, post_graphql_raw},
    config::Configs,
    gql::mutations::{self, ServiceInstanceUpdate},
    util::cac_deprecation::{find_all_cac_files, find_cac_file},
};

use super::*;

#[derive(Parser)]
pub struct MigrateArgs {
    /// Write files and clear Railway Config File settings (default is dry-run).
    #[clap(long)]
    apply: bool,

    /// Overwrite an existing `.railway/railway.ts`.
    #[clap(long)]
    force: bool,

    /// Delete discovered `railway.json` / `railway.toml` after a successful apply.
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
    nixpacks_config_path: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CacDeploy {
    start_command: Option<String>,
    pre_deploy_command: Option<JsonValue>,
    healthcheck_path: Option<String>,
    healthcheck_timeout: Option<i64>,
    restart_policy_type: Option<String>,
    restart_policy_max_retries: Option<i64>,
    num_replicas: Option<i64>,
    region: Option<String>,
    multi_region_config: Option<JsonValue>,
    cron_schedule: Option<String>,
    sleep_application: Option<bool>,
    draining_seconds: Option<i64>,
    overlap_seconds: Option<i64>,
}

struct CacService {
    name: String,
    path: PathBuf,
    service_id: Option<String>,
    cac: CacFile,
}

pub async fn migrate_config(args: MigrateArgs) -> Result<()> {
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

    for service in &services {
        eprintln!(
            "{} {} → {}",
            "Found".dimmed(),
            display_rel(&cwd, &service.path).cyan(),
            service.name.cyan()
        );
    }
    if services.len() > 1 {
        eprintln!(
            "{} {} services into one {}",
            "Merging".dimmed(),
            services.len().to_string().cyan(),
            format!(".railway/railway.{ext}").cyan()
        );
    } else {
        eprintln!(
            "{} service {}",
            "Migrating".dimmed(),
            services[0].name.cyan()
        );
    }

    if !args.apply {
        println!("{emitted}");
        eprintln!(
            "\n{} Dry-run only. Re-run with {} to write {} and clear Railway Config File settings.",
            "Note:".yellow().bold(),
            "railway config migrate --apply".cyan(),
            format!(".railway/railway.{ext}").cyan()
        );
        eprintln!(
            "  {} Review with {} then {}",
            "→".cyan(),
            "railway config plan".cyan(),
            "railway config apply".cyan()
        );
        return Ok(());
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
        "{} {}",
        "Wrote".green().bold(),
        railway_file.display().to_string().cyan()
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

    clear_railway_config_files(&services).await?;

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
    }

    eprintln!(
        "\n{} Run {} then {}.",
        "Next:".dimmed(),
        "railway config plan".cyan(),
        "railway config apply".cyan()
    );
    Ok(())
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
            service_id: Some(meta.id.clone()),
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
            service_id: None,
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
    id: String,
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
        let rel = config_file.trim_start_matches("./");
        let name = names
            .get(id)
            .cloned()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| guess_service_name(root, Path::new(rel)));
        index.insert(
            rel.to_string(),
            EnvCacMeta {
                id: id.clone(),
                name,
            },
        );
    }
    Ok(index)
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
            "\n# Last resort for a per-service CaC repo. Prefer one .railway file for the\n# project and drop this if you later combine services into that file.\nPARTIAL = {}\n",
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
            "\n// Last resort for a per-service CaC repo. Prefer one .railway file for the\n// project and drop this if you later combine services into that file.\nconst Partial = {}\n",
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
            "\n// Last resort for a per-service CaC repo. Prefer one .railway file for the\n// project and drop this if you later combine services into that file.\nexport const partial = {};\n",
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

async fn clear_railway_config_files(services: &[CacService]) -> Result<()> {
    let configs = Configs::new()?;
    let linked = match configs.get_linked_project().await {
        Ok(linked) => linked,
        Err(_) => {
            eprintln!(
                "{} No linked project — skipped clearing Railway Config File. Clear it in the dashboard if set.",
                "Warning:".yellow().bold()
            );
            return Ok(());
        }
    };

    let Some(environment_id) = linked.environment.clone() else {
        eprintln!(
            "{} No linked environment — skipped clearing Railway Config File.",
            "Warning:".yellow().bold()
        );
        return Ok(());
    };

    let mut ids: Vec<(String, String)> = services
        .iter()
        .filter_map(|service| {
            service
                .service_id
                .clone()
                .map(|id| (service.name.clone(), id))
        })
        .collect();
    if ids.is_empty() {
        if let Some(service_id) = linked.service.clone() {
            let name = services
                .first()
                .map(|service| service.name.clone())
                .unwrap_or_else(|| "linked service".to_string());
            ids.push((name, service_id));
        }
    }
    if ids.is_empty() {
        eprintln!(
            "{} No service IDs to clear — skipped clearing Railway Config File.",
            "Warning:".yellow().bold()
        );
        return Ok(());
    }

    let client = GQLClient::new_authorized(&configs)?;
    for (name, service_id) in ids {
        let input = mutations::service_instance_update::ServiceInstanceUpdateInput {
            railway_config_file: Some(String::new()),
            ..Default::default()
        };
        let vars = mutations::service_instance_update::Variables {
            service_id,
            environment_id: Some(environment_id.clone()),
            input,
        };
        post_graphql::<ServiceInstanceUpdate, _>(&client, configs.get_backboard(), vars)
            .await
            .with_context(|| {
                format!(
                    "Failed to clear railwayConfigFile on {name}. Clear it in the dashboard if set."
                )
            })?;
        eprintln!(
            "{} Cleared Railway Config File on {}",
            "Updated".green().bold(),
            name.cyan()
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
            service_id: None,
            cac,
        }
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
}
