//! Migrate Config as Code (`railway.json` / `railway.toml`) into
//! `.railway/railway.ts`. CaC → graph/DSL translation lives in the CLI only.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use colored::Colorize;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::{
    client::{GQLClient, post_graphql},
    config::Configs,
    gql::mutations::{self, ServiceInstanceUpdate},
    util::cac_deprecation::find_cac_file,
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

    /// Service name to emit in the DSL (defaults to directory name).
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

pub async fn migrate_config(args: MigrateArgs) -> Result<()> {
    if !matches!(args.lang.as_str(), "ts" | "py" | "go") {
        bail!("--lang must be one of: ts, py, go");
    }
    if args.delete_files && !args.apply {
        bail!("--delete-files requires --apply.");
    }

    let cwd = std::env::current_dir().context("Unable to get current directory")?;
    let cac_path = find_cac_file(&cwd)
        .context("No railway.json or railway.toml found in this directory or its parents.")?;

    let cac = parse_cac_file(&cac_path)?;
    let service_name = args
        .service
        .clone()
        .unwrap_or_else(|| guess_service_name(&cwd, &cac_path));

    let railway_dir = cwd.join(".railway");
    let ext = match args.lang.as_str() {
        "py" => "py",
        "go" => "go",
        _ => "ts",
    };
    let railway_file = railway_dir.join(format!("railway.{ext}"));
    let emitted = match args.lang.as_str() {
        "py" => emit_railway_py(&service_name, &cac),
        "go" => emit_railway_go(&service_name, &cac),
        _ => emit_railway_ts(&service_name, &cac),
    };

    eprintln!(
        "{} {}",
        "Found".dimmed(),
        cac_path.display().to_string().cyan()
    );
    eprintln!("{} service {}", "Migrating".dimmed(), service_name.cyan());

    if !args.apply {
        println!("{emitted}");
        eprintln!(
            "\n{} Dry-run only. Re-run with {} to write {} and clear the Railway Config File setting.",
            "Note:".yellow().bold(),
            "railway config migrate --apply".cyan(),
            ".railway/railway.{ts,py,go}".cyan()
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

    clear_railway_config_file_on_linked_service().await?;

    if args.delete_files {
        fs::remove_file(&cac_path)
            .with_context(|| format!("Failed to delete {}", cac_path.display()))?;
        eprintln!(
            "{} {}",
            "Deleted".green().bold(),
            cac_path.display().to_string().cyan()
        );
    }

    eprintln!(
        "\n{} Run {} then {}.",
        "Next:".dimmed(),
        "railway config plan".cyan(),
        "railway config apply".cyan()
    );
    Ok(())
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

fn emit_railway_py(service_name: &str, cac: &CacFile) -> String {
    let mut kwargs = Vec::new();
    if let Some(cmd) = &cac.build.build_command {
        kwargs.push(format!("        build={}", js_string(cmd)));
    }
    if let Some(cmd) = &cac.deploy.start_command {
        kwargs.push(format!("        start={}", js_string(cmd)));
    }
    if let Some(path) = &cac.deploy.healthcheck_path {
        kwargs.push(format!("        healthcheck={}", js_string(path)));
    }
    let service_call = if kwargs.is_empty() {
        format!("    web = service({})", js_string(service_name))
    } else {
        format!(
            "    web = service(\n        {},\n{},\n    )",
            js_string(service_name),
            kwargs.join(",\n")
        )
    };
    format!(
        r#"from railway_iac import define_railway, project, service

# Last resort for a per-service CaC repo. Prefer one .railway file for the
# project and drop this if you later combine services into that file.
PARTIAL = {project}

@define_railway
def main(ctx=None):
{service_call}
    return project({project}, resources=[web])
"#,
        service_call = service_call,
        project = js_string(service_name),
    )
}

fn emit_railway_go(service_name: &str, cac: &CacFile) -> String {
    let mut fields = Vec::new();
    if let Some(cmd) = &cac.build.build_command {
        fields.push(format!("\t\t\"build\": {},", js_string(cmd)));
    }
    if let Some(cmd) = &cac.deploy.start_command {
        fields.push(format!("\t\t\"start\": {},", js_string(cmd)));
    }
    if let Some(path) = &cac.deploy.healthcheck_path {
        fields.push(format!("\t\t\"healthcheck\": {},", js_string(path)));
    }
    let config_block = if fields.is_empty() {
        "nil".to_string()
    } else {
        format!("iac.ServiceConfig{{\n{}\n\t}}", fields.join("\n"))
    };
    format!(
        r#"package main

import "github.com/railwayapp/railway-go-iac/iac"

// Last resort for a per-service CaC repo. Prefer one .railway file for the
// project and drop this if you later combine services into that file.
const Partial = {name}

func Railway() iac.Project {{
	web := iac.ServiceNamed({name}, {config})
	return iac.ProjectNamed({name}, []any{{web}})
}}
"#,
        name = js_string(service_name),
        config = config_block,
    )
}

fn emit_railway_ts(service_name: &str, cac: &CacFile) -> String {
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
        // Single-region placement via replicas map is the IaC form.
        fields.push(format!("    replicas: {{ {}: 1 }},", js_string(region)));
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
    if let Some(pre) = &cac.deploy.pre_deploy_command {
        fields.push(format!(
            "    // preDeployCommand from CaC: {}",
            json_to_ts(pre)
        ));
    }
    if let Some(watch) = &cac.build.watch_patterns {
        let arr = watch
            .iter()
            .map(|p| js_string(p))
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(format!("    // watchPatterns from CaC: [{arr}]"));
    }

    let body = if fields.is_empty() {
        format!("  const web = service({});\n", js_string(service_name))
    } else {
        format!(
            "  const web = service({}, {{\n{}\n  }});\n",
            js_string(service_name),
            fields.join("\n")
        )
    };

    format!(
        r#"import {{ defineRailway, project, service }} from "railway/iac";

// Last resort for a per-service CaC repo. Prefer one .railway file for the
// project and drop this if you later combine services into that file.
export const partial = {project};

export default defineRailway(() => {{
{body}
  return project({project}, {{
    resources: [web],
  }});
}});
"#,
        body = body,
        project = js_string(service_name),
    )
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

async fn clear_railway_config_file_on_linked_service() -> Result<()> {
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
    let Some(service_id) = linked.service.clone() else {
        eprintln!(
            "{} No linked service — skipped clearing Railway Config File.",
            "Warning:".yellow().bold()
        );
        return Ok(());
    };

    let Some(environment_id) = linked.environment.clone() else {
        eprintln!(
            "{} No linked environment — skipped clearing Railway Config File.",
            "Warning:".yellow().bold()
        );
        return Ok(());
    };

    let client = GQLClient::new_authorized(&configs)?;
    let input = mutations::service_instance_update::ServiceInstanceUpdateInput {
        // Empty string clears the dashboard config-file path.
        railway_config_file: Some(String::new()),
        ..Default::default()
    };
    let vars = mutations::service_instance_update::Variables {
        service_id,
        environment_id: Some(environment_id),
        input,
    };

    post_graphql::<ServiceInstanceUpdate, _>(&client, configs.get_backboard(), vars)
        .await
        .context(
            "Failed to clear railwayConfigFile on the linked service. Clear it in the dashboard if set.",
        )?;

    eprintln!(
        "{} Cleared Railway Config File on the linked service",
        "Updated".green().bold()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let out = emit_railway_ts("api", &cac);
        assert!(out.contains("build: \"pnpm build\""));
        assert!(out.contains("start: \"pnpm start\""));
        assert!(out.contains("healthcheck: \"/health\""));
        assert!(out.contains("service(\"api\""));
        assert!(out.contains("export const partial = \"api\""));
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
