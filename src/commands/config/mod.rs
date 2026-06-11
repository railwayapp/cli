mod runner;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use is_terminal::IsTerminal;

use crate::util::prompt::{prompt_confirm_with_default, prompt_select};

use super::*;

/// Define, import, preview, and apply your Railway project from .railway/railway.ts
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser)]
enum Command {
    /// Preview the changes Railway would make from .railway/railway.ts without applying them
    Plan(SharedArgs),

    /// Staged Railway configuration changes are not available yet; use `railway config plan` or `railway config apply`
    #[clap(hide = true)]
    Stage(SharedArgs),

    /// Apply the changes from .railway/railway.ts to the linked Railway project
    Apply(SharedArgs),

    /// Create .railway/railway.ts for this repo or import from the linked project
    Init(InitArgs),

    /// Import the linked Railway project's current configuration into .railway/railway.ts
    Pull(PullArgs),
}

#[derive(Parser, Clone)]
struct SharedArgs {
    /// Path to the Railway configuration file. Defaults to nearest .railway/railway.ts.
    #[clap(long)]
    file: Option<PathBuf>,

    /// Output raw runner JSON.
    #[clap(long)]
    json: bool,

    /// Confirm prompts and proceed non-interactively.
    #[clap(long)]
    yes: bool,

    /// Allow destructive applies in non-interactive or agent sessions.
    #[clap(long)]
    confirm_destructive: bool,

    /// Ask Railway to decrypt variables while planning, when authorized.
    #[clap(long)]
    decrypt_variables: bool,

    /// Include generated graph TypeScript types in runner output.
    #[clap(long)]
    include_types: bool,

    /// Path to the TypeScript configuration runner. Defaults to RAILWAY_IAC_TS_BIN or railway-iac-ts.
    #[clap(long)]
    runner: Option<String>,

    /// Show full change details.
    #[clap(long, alias = "full")]
    verbose: bool,
}

#[derive(Clone, Copy)]
enum InitMode {
    GenerateFromRepo,
    ImportFromRailway,
    MinimalFile,
}

impl std::fmt::Display for InitMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitMode::GenerateFromRepo => {
                write!(f, "Scan this directory and suggest a basic setup")
            }
            InitMode::ImportFromRailway => write!(f, "Import an existing Railway project"),
            InitMode::MinimalFile => write!(f, "Create an empty configuration file"),
        }
    }
}

#[derive(Parser)]
struct InitArgs {
    /// Overwrite an existing .railway/railway.ts file.
    #[clap(long)]
    force: bool,
}

#[derive(Parser)]
struct PullArgs {
    /// Overwrite an existing .railway/railway.ts file.
    #[clap(long)]
    force: bool,

    /// Output raw imported graph JSON instead of writing files.
    #[clap(long)]
    json: bool,

    /// Path to the TypeScript configuration runner. Defaults to RAILWAY_IAC_TS_BIN or railway-iac-ts.
    #[clap(long)]
    runner: Option<String>,

    /// Ask an agent to turn imported state into idiomatic railway.ts code.
    #[clap(long)]
    agent: bool,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Command::Plan(args) => {
            if args.yes {
                bail!("--yes is only valid with `railway config apply`.");
            }
            if args.confirm_destructive {
                bail!("--confirm-destructive is only valid with `railway config apply`.");
            }
            run_sync(args, false, false).await
        }
        Command::Stage(_args) => bail!(
            "Staged Railway configuration changes are not available yet. Run `railway config plan` to preview changes or `railway config apply` to apply them."
        ),
        Command::Apply(args) => run_sync(args, false, true).await,
        Command::Init(args) => init_config(args).await,
        Command::Pull(args) => pull_config(args).await,
    }
}

async fn init_config(args: InitArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("Unable to get current directory")?;
    let project_name = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("railway-project")
        .to_string();

    let railway_dir = cwd.join(".railway");
    let railway_file = railway_dir.join("railway.ts");
    let readme_file = railway_dir.join("README.md");
    let skill_dir = cwd.join(".agents").join("skills").join("railway-config");
    let skill_file = skill_dir.join("SKILL.md");

    create_parent(&railway_file)?;
    create_parent(&skill_file)?;

    let init_mode = if railway_file.exists() || !std::io::stdout().is_terminal() {
        InitMode::GenerateFromRepo
    } else {
        println!();
        println!("{}", "Initialize Railway configuration".bold());
        println!("Railway will create the files that define your project infrastructure as code.");
        println!("{} {}", "Main file".dimmed(), ".railway/railway.ts".cyan());
        println!(
            "{} {}",
            "Docs".dimmed(),
            "https://docs.railway.com/infrastructure-as-code".cyan()
        );
        println!();
        prompt_select(
            "How should Railway start?",
            vec![
                InitMode::GenerateFromRepo,
                InitMode::ImportFromRailway,
                InitMode::MinimalFile,
            ],
        )?
    };

    match init_mode {
        InitMode::GenerateFromRepo => write_new(
            &railway_file,
            &railway_ts_from_repo(&cwd, &project_name),
            args.force,
        )?,
        InitMode::ImportFromRailway => write_pulled_config(&railway_file, args.force, None).await?,
        InitMode::MinimalFile => write_new(&railway_file, &railway_ts(&project_name), args.force)?,
    }
    write_new(
        &readme_file,
        include_str!("../../../assets/railway-config/README.md"),
        args.force,
    )?;
    let wrote_skill = write_asset_if_missing(
        &skill_file,
        include_str!("../../../assets/railway-config/SKILL.md"),
    )?;

    println!("{}", "Railway configuration initialized".green().bold());
    println!(
        "{} {}",
        match init_mode {
            InitMode::ImportFromRailway => "Imported",
            _ => "Created",
        }
        .dimmed(),
        railway_file.display().to_string().cyan()
    );
    println!(
        "{} {}",
        "Created".dimmed(),
        readme_file.display().to_string().cyan()
    );
    if wrote_skill {
        println!(
            "{} {}",
            "Created".dimmed(),
            skill_file.display().to_string().cyan()
        );
    }
    println!();
    println!("{}", "Next steps".bold());
    println!(
        "  {} Edit {} to describe your Railway project.",
        "•".cyan(),
        ".railway/railway.ts".cyan()
    );
    println!(
        "  {} Run {} to preview changes.",
        "•".cyan(),
        "railway config plan".cyan()
    );
    println!(
        "  {} Run {} to apply them.",
        "•".cyan(),
        "railway config apply".cyan()
    );
    println!(
        "  {} Read the guide and reference at {}.",
        "•".cyan(),
        "https://docs.railway.com/infrastructure-as-code".cyan()
    );

    Ok(())
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    Ok(())
}

fn write_asset_if_missing(path: &Path, contents: &str) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    fs::write(path, contents).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(true)
}

fn write_new(path: &Path, contents: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists. Re-run with --force to overwrite it.",
            path.display()
        );
    }
    fs::write(path, contents).with_context(|| format!("Failed to write {}", path.display()))
}

async fn pull_config(args: PullArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("Unable to get current directory")?;
    let railway_file = cwd.join(".railway").join("railway.ts");
    let readme_file = cwd.join(".railway").join("README.md");
    let skill_file = cwd
        .join(".agents")
        .join("skills")
        .join("railway-config")
        .join("SKILL.md");

    if args.json {
        let graph = load_current_graph(args.runner).await?;
        println!("{}", serde_json::to_string_pretty(&graph)?);
        return Ok(());
    }

    create_parent(&railway_file)?;
    create_parent(&skill_file)?;
    write_pulled_config(&railway_file, args.force, args.runner).await?;
    let wrote_readme = write_asset_if_missing(
        &readme_file,
        include_str!("../../../assets/railway-config/README.md"),
    )?;
    let wrote_skill = write_asset_if_missing(
        &skill_file,
        include_str!("../../../assets/railway-config/SKILL.md"),
    )?;

    println!("{}", "Railway configuration imported".green().bold());
    println!(
        "{} {}",
        "Updated".dimmed(),
        railway_file.display().to_string().cyan()
    );
    if wrote_readme {
        println!(
            "{} {}",
            "Created".dimmed(),
            readme_file.display().to_string().cyan()
        );
    }
    if wrote_skill {
        println!(
            "{} {}",
            "Created".dimmed(),
            skill_file.display().to_string().cyan()
        );
    }
    println!();
    println!("{}", "Next steps".bold());
    println!(
        "  {} Review {} and remove anything you do not want managed from code.",
        "•".cyan(),
        ".railway/railway.ts".cyan()
    );
    println!(
        "  {} Run {} to verify it matches Railway.",
        "•".cyan(),
        "railway config plan".cyan()
    );
    if args.agent {
        println!(
            "  {} Ask your agent to clean this import into idiomatic Railway configuration.",
            "•".cyan()
        );
    }

    Ok(())
}

async fn write_pulled_config(path: &Path, force: bool, runner: Option<String>) -> Result<()> {
    let graph = load_current_graph(runner).await?;
    write_new(path, &render_graph_as_railway_ts(&graph), force)
}

async fn load_current_graph(runner: Option<String>) -> Result<runner::DesiredGraph> {
    let temp_dir = std::env::temp_dir().join(format!("railway-config-pull-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).context("Failed to create temporary Railway config directory")?;
    let temp_file = temp_dir.join("railway.ts");
    fs::write(&temp_file, railway_ts("import-placeholder"))
        .context("Failed to write temporary Railway config")?;

    let args = runner::Args {
        file: Some(temp_file.clone()),
        stage: false,
        json: true,
        yes: false,
        confirm_destructive: false,
        apply: false,
        decrypt_variables: false,
        include_types: false,
        runner,
        verbose: false,
    };
    let response = runner::run(&args, "current").await?;
    let _ = fs::remove_file(temp_file);
    let _ = fs::remove_dir(temp_dir);

    if !response.ok {
        bail!("Could not import Railway configuration because planning returned diagnostics.");
    }

    response
        .current_graph
        .context("Railway did not return current project state")
}

fn render_graph_as_railway_ts(graph: &runner::DesiredGraph) -> String {
    let mut imports = vec!["defineRailway", "project", "service"];
    if graph
        .resources
        .iter()
        .any(|resource| resource.r#type == "bucket")
    {
        imports.push("bucket");
    }
    if graph.resources.iter().any(|resource| {
        resource
            .source
            .as_ref()
            .and_then(|source| source.get("repo"))
            .is_some()
    }) {
        imports.push("github");
    }
    if graph.resources.iter().any(|resource| {
        resource
            .source
            .as_ref()
            .and_then(|source| source.get("image"))
            .is_some()
            && resource.r#type == "service"
    }) {
        imports.push("image");
    }
    // Imported unknown secrets are preserved by default and omitted from generated source.
    if graph.resources.iter().any(|resource| {
        resource.r#type == "database" && resource.engine.as_deref() == Some("postgres")
    }) {
        imports.push("postgres");
    }
    if graph.resources.iter().any(|resource| {
        resource.r#type == "database" && resource.engine.as_deref() == Some("redis")
    }) {
        imports.push("redis");
    }
    if graph.resources.iter().any(|resource| {
        resource.r#type == "database" && resource.engine.as_deref() == Some("mysql")
    }) {
        imports.push("mysql");
    }
    if graph.resources.iter().any(|resource| {
        resource.r#type == "database" && resource.engine.as_deref() == Some("mongo")
    }) {
        imports.push("mongo");
    }
    imports.sort();
    imports.dedup();

    let mut out = format!(
        "import {{ {} }} from \"railway/iac\";\n\n",
        imports.join(", ")
    );
    out.push_str("export default defineRailway(() => {\n");

    let source_aliases = shared_github_sources(graph);
    for (alias, repo) in &source_aliases {
        out.push_str(&format!("  const {alias} = github({:?});\n", repo));
    }
    if !source_aliases.is_empty() {
        out.push('\n');
    }

    let mut names = Vec::new();
    let import_names: std::collections::HashSet<&str> = imports.iter().copied().collect();
    for resource in &graph.resources {
        let var_name =
            unique_resource_ident(&resource.name, &resource.r#type, &import_names, &names);
        match resource.r#type.as_str() {
            "database" => {
                let helper = match resource.engine.as_deref() {
                    Some("postgres") => "postgres",
                    Some("redis") => "redis",
                    Some("mysql") => "mysql",
                    Some("mongo") => "mongo",
                    _ => "service",
                };
                if helper == "service" {
                    out.push_str(&format!(
                        "  const {var_name} = service(\"{}\");\n",
                        resource.name
                    ));
                } else {
                    out.push_str(&format!(
                        "  const {var_name} = {helper}(\"{}\");\n",
                        resource.name
                    ));
                }
                names.push(var_name);
            }
            "service" => {
                out.push_str(&format!(
                    "  const {var_name} = service(\"{}\"",
                    resource.name
                ));
                let body = render_service_body(resource, &source_aliases);
                if body.is_empty() {
                    out.push_str(");\n");
                } else {
                    out.push_str(&format!(", {body});\n"));
                }
                names.push(var_name);
            }
            "bucket" => {
                let config = resource.config.as_ref().map(ts_value).unwrap_or_default();
                if config.is_empty() {
                    out.push_str(&format!(
                        "  const {var_name} = bucket(\"{}\");\n",
                        resource.name
                    ));
                } else {
                    out.push_str(&format!(
                        "  const {var_name} = bucket(\"{}\", {config});\n",
                        resource.name
                    ));
                }
                names.push(var_name);
            }
            _ => {}
        }
    }

    let project_name = graph
        .project
        .as_ref()
        .map(|project| project.name.as_str())
        .unwrap_or("imported-project");
    out.push_str(&format!("\n  return project({:?}, {{\n", project_name));
    out.push_str(&format!("    resources: [{}],\n", names.join(", ")));
    out.push_str("  });\n");
    out.push_str("});\n");
    out
}

fn shared_github_sources(
    graph: &runner::DesiredGraph,
) -> std::collections::BTreeMap<String, String> {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for resource in &graph.resources {
        if resource.r#type != "service" {
            continue;
        }
        if let Some(repo) = resource
            .source
            .as_ref()
            .and_then(|source| source.get("repo"))
            .and_then(|value| value.as_str())
        {
            *counts.entry(repo.to_string()).or_default() += 1;
        }
    }

    let reserved = std::collections::HashSet::from([
        "defineRailway",
        "project",
        "service",
        "github",
        "image",
        "bucket",
    ]);
    let mut used = Vec::new();
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(repo, _)| {
            let repo_name = repo.rsplit('/').next().unwrap_or(&repo);
            let alias = unique_resource_ident(repo_name, "source", &reserved, &used);
            used.push(alias.clone());
            (alias, repo)
        })
        .collect()
}

fn render_service_body(
    resource: &runner::DesiredResource,
    source_aliases: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut lines = Vec::new();
    if let Some(source) = &resource.source {
        if let Some(repo) = source.get("repo").and_then(|value| value.as_str()) {
            let alias = source_aliases
                .iter()
                .find_map(|(alias, shared_repo)| (shared_repo == repo).then_some(alias));
            let root = source
                .get("rootDirectory")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty());
            let branch = source
                .get("branch")
                .and_then(|value| value.as_str())
                .filter(|branch| *branch != "main");
            if let Some(alias) = alias {
                lines.push(format!("    source: {alias},"));
                if let Some(root) = root {
                    lines.push(format!("    root: {:?},", root));
                }
            } else {
                let mut options = Vec::new();
                if let Some(branch) = branch {
                    options.push(format!("branch: {:?}", branch));
                }
                if let Some(root) = root {
                    options.push(format!("rootDirectory: {:?}", root));
                }
                let args = if options.is_empty() {
                    format!("{:?}", repo)
                } else {
                    format!("{:?}, {{ {} }}", repo, options.join(", "))
                };
                lines.push(format!("    source: github({args}),"));
            }
        } else if let Some(image_name) = source.get("image").and_then(|value| value.as_str()) {
            lines.push(format!("    source: image({:?}),", image_name));
        }
    }
    render_build(resource.build.as_ref(), &mut lines);
    render_deploy(resource.deploy.as_ref(), &mut lines);
    render_networking(resource.networking.as_ref(), &mut lines);
    render_variables(resource.variables.as_ref(), &mut lines);
    if lines.is_empty() {
        return String::new();
    }
    format!("{{\n{}\n  }}", lines.join("\n"))
}

fn render_variables(
    vars: Option<&serde_json::Map<String, serde_json::Value>>,
    lines: &mut Vec<String>,
) {
    let Some(vars) = vars else {
        return;
    };
    let mut entries = vars.iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    let rendered = entries
        .into_iter()
        .filter_map(|(key, value)| {
            if value.get("type").and_then(|value| value.as_str()) == Some("preserve") {
                return None;
            }
            if let Some(literal) = value.get("value").and_then(|value| value.as_str()) {
                return Some(format!("      {}: {:?},", ts_key(key), literal));
            }
            if let (Some(resource), Some(output)) = (
                value.get("resource").and_then(|value| value.as_str()),
                value.get("output").and_then(|value| value.as_str()),
            ) {
                let name = resource.split('.').skip(1).collect::<Vec<_>>().join(".");
                return Some(format!(
                    "      {}: /* {}.{} */ \"${{{{{}}}}}\",",
                    ts_key(key),
                    name,
                    output,
                    output
                ));
            }
            None
        })
        .collect::<Vec<_>>();

    if rendered.is_empty() {
        return;
    }
    lines.push("    env: {".to_string());
    lines.extend(rendered);
    lines.push("    },".to_string());
}

fn render_build(build: Option<&serde_json::Value>, lines: &mut Vec<String>) {
    let Some(build) = build else {
        return;
    };
    if let Some(object) = build.as_object() {
        let non_default_keys = object
            .iter()
            .filter(|(key, value)| {
                !matches!((key.as_str(), value),
                    ("builder", serde_json::Value::String(builder)) if builder == "RAILPACK" || builder == "NIXPACKS"
                ) && !matches!((key.as_str(), value),
                    ("buildEnvironment", serde_json::Value::String(environment)) if environment == "V3"
                )
            })
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>();
        if non_default_keys.is_empty() {
            return;
        }
        if non_default_keys == ["buildCommand"] {
            if let Some(command) = build.get("buildCommand").and_then(|value| value.as_str()) {
                lines.push(format!("    build: {:?},", command));
                return;
            }
        }
    }
    if !is_empty_object(build) {
        lines.push(format!("    build: {},", ts_value(build)));
    }
}

fn render_deploy(deploy: Option<&serde_json::Value>, lines: &mut Vec<String>) {
    let Some(deploy) = deploy.and_then(|value| value.as_object()) else {
        return;
    };
    let mut remaining = deploy.clone();

    if let Some(start) = remaining
        .remove("startCommand")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
    {
        lines.push(format!("    start: {:?},", start));
    }
    if let Some(healthcheck) = remaining
        .remove("healthcheckPath")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
    {
        lines.push(format!("    healthcheck: {:?},", healthcheck));
    }
    if let Some(timeout) = remaining.remove("healthcheckTimeout") {
        lines.push(format!("    healthcheckTimeout: {},", ts_value(&timeout)));
    }
    if let Some(regions) = remaining.remove("multiRegionConfig") {
        lines.push(format!("    replicas: {},", render_replicas(&regions)));
    }

    if remaining
        .get("ipv6EgressEnabled")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        remaining.remove("ipv6EgressEnabled");
    }
    if remaining.get("runtime").and_then(|value| value.as_str()) == Some("V2") {
        remaining.remove("runtime");
    }
    if remaining
        .get("useLegacyStacker")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        remaining.remove("useLegacyStacker");
    }

    if !remaining.is_empty() {
        lines.push(format!(
            "    deploy: {},",
            ts_value(&serde_json::Value::Object(remaining))
        ));
    }
}

fn render_replicas(value: &serde_json::Value) -> String {
    let Some(regions) = value.as_object() else {
        return ts_value(value);
    };
    let active = regions
        .iter()
        .filter_map(|(region, config)| {
            let replicas = config.get("numReplicas").and_then(|value| value.as_u64())?;
            Some((region, config, replicas))
        })
        .collect::<Vec<_>>();
    if active.len() == 1 {
        return active[0].2.to_string();
    }
    render_regions(value)
}

fn render_regions(value: &serde_json::Value) -> String {
    let Some(regions) = value.as_object() else {
        return ts_value(value);
    };
    let rendered = regions
        .iter()
        .map(|(region, config)| {
            let replicas = config.get("numReplicas").and_then(|value| value.as_u64());
            let stacker = config
                .get("stackerAssignment")
                .and_then(|value| value.as_str());
            let value = match (replicas, stacker) {
                (Some(replicas), None) => replicas.to_string(),
                _ => {
                    let mut parts = Vec::new();
                    if let Some(replicas) = replicas {
                        parts.push(format!("count: {replicas}"));
                    }
                    if let Some(stacker) = stacker {
                        parts.push(format!("stacker: {:?}", stacker));
                    }
                    format!("{{ {} }}", parts.join(", "))
                }
            };
            format!("{:?}: {value}", region)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{ {rendered} }}")
}

fn render_networking(networking: Option<&serde_json::Value>, lines: &mut Vec<String>) {
    let Some(networking) = networking.and_then(|value| value.as_object()) else {
        return;
    };
    let mut remaining = networking.clone();

    remaining.remove("serviceDomains");

    if let Some(custom_domains) = remaining.remove("customDomains") {
        if let Some(domains) = custom_domains
            .as_object()
            .filter(|domains| !domains.is_empty())
        {
            let rendered = domains
                .iter()
                .map(|(domain, config)| {
                    let port = config.get("port").and_then(|value| value.as_u64());
                    match port {
                        Some(8080) | None => format!("{:?}", domain),
                        Some(port) => format!("{{ domain: {:?}, port: {port} }}", domain),
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("    domains: [{rendered}],"));
        }
    }

    if !remaining.is_empty() {
        lines.push(format!(
            "    networking: {},",
            ts_value(&serde_json::Value::Object(remaining))
        ));
    }
}

fn is_empty_object(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|object| object.is_empty())
}

fn ts_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(object) => {
            if object.is_empty() {
                return "{}".to_string();
            }
            let fields = object
                .iter()
                .map(|(key, value)| format!("{}: {}", ts_key(key), ts_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {fields} }}")
        }
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values.iter().map(ts_value).collect::<Vec<_>>().join(", ")
        ),
        serde_json::Value::String(value) => format!("{:?}", value),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Null => "null".to_string(),
    }
}

fn ts_key(key: &str) -> String {
    if key
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        key.to_string()
    } else {
        format!("{:?}", key)
    }
}

fn unique_resource_ident(
    name: &str,
    resource_type: &str,
    reserved: &std::collections::HashSet<&str>,
    used: &[String],
) -> String {
    let mut candidate = sanitize_ident(name);
    if candidate.is_empty() || reserved.contains(candidate.as_str()) {
        candidate = match resource_type {
            "database" => format!("{}Database", candidate),
            "service" => format!("{}Service", candidate),
            _ => format!("{}Resource", candidate),
        };
    }
    if candidate.is_empty()
        || candidate == "Database"
        || candidate == "Service"
        || candidate == "Resource"
    {
        candidate = "resource".to_string();
    }
    let base = candidate.clone();
    let mut suffix = 2;
    while used.iter().any(|name| name == &candidate) || reserved.contains(candidate.as_str()) {
        candidate = format!("{base}{suffix}");
        suffix += 1;
    }
    candidate
}

fn sanitize_ident(name: &str) -> String {
    let mut out = String::new();
    let mut capitalize_next = false;
    for (idx, ch) in name.chars().enumerate() {
        if ch == '-' || ch == ' ' || ch == '.' {
            capitalize_next = true;
            continue;
        }
        if idx == 0 && !(ch.is_ascii_alphabetic() || ch == '_') {
            out.push('_');
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if capitalize_next {
                out.push(ch.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                out.push(ch);
            }
        }
    }
    out
}

fn railway_ts_from_repo(cwd: &Path, project_name: &str) -> String {
    let package_json = cwd.join("package.json");
    if !package_json.exists() {
        return railway_ts(project_name);
    }

    let package = fs::read_to_string(package_json)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        .unwrap_or_default();
    let scripts = package
        .get("scripts")
        .and_then(|scripts| scripts.as_object());
    let package_manager = detect_package_manager(cwd);
    let build = script_command(scripts, "build").map(|_| format!("{package_manager} run build"));
    let start = script_command(scripts, "start")
        .map(ToOwned::to_owned)
        .or_else(|| {
            if cwd.join("src/index.ts").exists() && package_manager == "bun" {
                Some("bun src/index.ts".to_string())
            } else if cwd.join("index.js").exists() {
                Some("node index.js".to_string())
            } else {
                None
            }
        });
    let github_source = detect_github_remote(cwd);

    let imports = if github_source.is_some() {
        "defineRailway, github, project, service"
    } else {
        "defineRailway, project, service"
    };
    let mut out = format!("import {{ {imports} }} from \"railway/iac\";\n\n");
    out.push_str("export default defineRailway(() => {\n");
    out.push_str("  const web = service(\"web\", {\n");
    if let Some(source) = github_source {
        out.push_str(&format!("    source: github({:?}),\n", source));
    } else {
        out.push_str(
            "    // No GitHub remote detected. `railway up` will upload this directory.\n",
        );
    }
    if let Some(build) = build {
        out.push_str(&format!("    build: {:?},\n", build));
    }
    if let Some(start) = start {
        out.push_str(&format!("    start: {:?},\n", start));
    }

    out.push_str("  });\n\n");
    out.push_str(&format!("  return project(\"{project_name}\", {{\n"));
    out.push_str("    resources: [web],\n  });\n});\n");
    out
}

fn script_command<'a>(
    scripts: Option<&'a serde_json::Map<String, serde_json::Value>>,
    name: &str,
) -> Option<&'a str> {
    scripts
        .and_then(|scripts| scripts.get(name))
        .and_then(|value| value.as_str())
}

fn detect_github_remote(cwd: &Path) -> Option<String> {
    let output = ProcessCommand::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_github_remote(std::str::from_utf8(&output.stdout).ok()?.trim())
}

fn parse_github_remote(remote: &str) -> Option<String> {
    let remote = remote.strip_suffix(".git").unwrap_or(remote);
    if let Some(path) = remote.strip_prefix("git@github.com:") {
        return Some(path.to_string());
    }
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
    ] {
        if let Some(path) = remote.strip_prefix(prefix) {
            return Some(path.to_string());
        }
    }
    None
}

fn detect_package_manager(cwd: &Path) -> String {
    if cwd.join("bun.lock").exists() || cwd.join("bun.lockb").exists() {
        "bun".to_string()
    } else if cwd.join("pnpm-lock.yaml").exists() {
        "pnpm".to_string()
    } else if cwd.join("yarn.lock").exists() {
        "yarn".to_string()
    } else {
        "npm".to_string()
    }
}

fn railway_ts(project_name: &str) -> String {
    format!(
        r#"import {{ defineRailway, project, service }} from "railway/iac";

export default defineRailway(() => {{
  const web = service("web", {{
    // Add build/start commands when Railway cannot infer them.
    // build: "pnpm install --frozen-lockfile && pnpm build",
    // start: "pnpm start",
    env: {{
      NODE_ENV: "production",
    }},
  }});

  return project("{project_name}", {{
    resources: [web],
  }});
}});
"#
    )
}

async fn run_sync(args: SharedArgs, stage: bool, apply: bool) -> Result<()> {
    ensure_config_initialized(&args).await?;

    runner::run_command(runner::Args {
        file: args.file,
        stage,
        json: args.json,
        yes: args.yes,
        confirm_destructive: args.confirm_destructive,
        apply,
        decrypt_variables: args.decrypt_variables,
        include_types: args.include_types,
        runner: args.runner,
        verbose: args.verbose,
    })
    .await
}

async fn ensure_config_initialized(args: &SharedArgs) -> Result<()> {
    if args.file.is_some() {
        return Ok(());
    }

    let cwd = std::env::current_dir().context("Unable to get current directory")?;
    let railway_file = cwd.join(".railway").join("railway.ts");
    if railway_file.exists() {
        return Ok(());
    }

    println!();
    println!("{}", "Railway configuration is not initialized yet.".bold());
    println!(
        "{} {}",
        "Create".dimmed(),
        railway_file.display().to_string().cyan()
    );
    println!();

    let should_init = if args.yes {
        true
    } else {
        if !std::io::stdout().is_terminal() {
            bail!("Railway configuration is not initialized. Run `railway config init` first.");
        }
        prompt_confirm_with_default("Initialize Railway configuration for this project?", false)?
    };

    if !should_init {
        bail!("Run `railway config init` to create .railway/railway.ts, then try again.");
    }

    init_config(InitArgs { force: false }).await?;
    println!();
    Ok(())
}
