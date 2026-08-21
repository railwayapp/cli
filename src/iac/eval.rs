use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::compiler::project_definition_to_graph;
use super::graph::RailwayGraph;
use super::partial::parse_partial_name;

pub struct EvaluatedFile {
    pub file: PathBuf,
    pub graph: RailwayGraph,
    pub partial: Option<String>,
}

pub fn evaluate_file(file: &Path) -> Result<EvaluatedFile> {
    let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let ext = file
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let payload = match ext.as_str() {
        "py" => evaluate_python(&file)?,
        "go" => evaluate_go(&file)?,
        "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" => evaluate_javascript(&file)?,
        other => bail!("Unsupported IaC file extension: .{other}"),
    };
    let partial = parse_partial_name(payload.get("partial").and_then(Value::as_str))
        .map_err(|err| anyhow::anyhow!(err))?;
    let project = payload
        .get("project")
        .cloned()
        .or_else(|| payload.get("graph").cloned())
        .unwrap_or(payload.clone());
    let graph = project_definition_to_graph(&normalize_project(project));
    Ok(EvaluatedFile {
        file,
        graph,
        partial,
    })
}

fn normalize_project(mut project: Value) -> Value {
    if project.get("resources").is_none() {
        if let Some(resources) = project.get("Resources").cloned() {
            project["resources"] = resources;
        }
    }
    if let Some(resources) = project.get_mut("resources").and_then(Value::as_array_mut) {
        for resource in resources {
            if resource.get("address").is_none() {
                let kind = resource
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("service");
                let name = resource
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("web");
                resource["address"] = json!(format!("{kind}.{name}"));
            }
            if resource.get("kind").is_none()
                && resource.get("type").and_then(Value::as_str) == Some("service")
            {
                resource["kind"] = json!("empty");
            }
            if let Some(start) = resource.get("start").cloned() {
                if resource.get("deploy").is_none() {
                    resource["deploy"] = json!({});
                }
                if resource["deploy"].get("startCommand").is_none() {
                    resource["deploy"]["startCommand"] = start;
                }
            }
            if let Some(build) = resource.get("build").cloned() {
                if build.is_string() {
                    resource["build"] = json!({ "buildCommand": build });
                }
            }
        }
    }
    if project.get("name").is_none() {
        project["name"] = json!("app");
    }
    project
}

fn evaluate_python(file: &Path) -> Result<Value> {
    let script = r#"
import importlib.util, json, sys
path = sys.argv[1]
spec = importlib.util.spec_from_file_location("railway_iac_user", path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
partial = getattr(mod, "PARTIAL", None) or getattr(mod, "Partial", None) or getattr(mod, "partial", None)
candidate = getattr(mod, "main", None) or getattr(mod, "Railway", None) or getattr(mod, "default", None)
project = candidate() if callable(candidate) else candidate
if hasattr(project, "to_graph"):
    project = project.to_graph()
print(json.dumps({"partial": partial, "project": project}, default=str))
"#;
    let output = Command::new("python3")
        .args(["-c", script, &file.to_string_lossy()])
        .output()
        .context("Failed to run python3 to evaluate .railway/railway.py")?;
    decode_eval_output("python3", &output)
}

fn evaluate_go(file: &Path) -> Result<Value> {
    let dir = file
        .parent()
        .context("Go IaC file has no parent directory")?;
    let wrapper = dir.join(".railway-iac-eval.go");
    let has_partial = fs::read_to_string(file)
        .unwrap_or_default()
        .contains("Partial");
    let source = if has_partial {
        r#"package main
import ("encoding/json"; "os")
func main() {
  out, err := json.Marshal(map[string]any{"partial": Partial, "project": Railway().Graph()})
  if err != nil { panic(err) }
  os.Stdout.Write(out)
}
"#
    } else {
        r#"package main
import ("encoding/json"; "os")
func main() {
  out, err := json.Marshal(map[string]any{"project": Railway().Graph()})
  if err != nil { panic(err) }
  os.Stdout.Write(out)
}
"#
    };
    fs::write(&wrapper, source)?;
    let output = Command::new("go")
        .args([
            "run",
            file.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("railway.go"),
            wrapper
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(".railway-iac-eval.go"),
        ])
        .current_dir(dir)
        .output();
    let _ = fs::remove_file(&wrapper);
    decode_eval_output(
        "go run",
        &output.context("Failed to run go to evaluate .railway/railway.go")?,
    )
}

fn evaluate_javascript(file: &Path) -> Result<Value> {
    let script = r#"
import { pathToFileURL } from "node:url";
const file = process.argv[1];
const mod = await import(`${pathToFileURL(file).href}?t=${Date.now()}`);
const partial = mod.partial ?? mod.PARTIAL ?? mod.Partial ?? undefined;
let exported = mod.default ?? mod.main ?? mod.Railway ?? mod;
while (exported && typeof exported === "object" && "default" in exported && exported.name == null && exported.resources == null) {
  exported = exported.default;
}
const project = typeof exported === "function" ? await exported({}) : exported;
const graph = project && typeof project === "object" && "to_graph" in project ? project.to_graph() : project;
process.stdout.write(JSON.stringify({ partial, project: graph }));
"#;
    let mut command = Command::new("node");
    command.args([
        "--experimental-strip-types",
        "--disable-warning=ExperimentalWarning",
        "--input-type=module",
        "-e",
        script,
        "--",
        &file.to_string_lossy(),
    ]);
    if let Some(modules) = nearest_node_modules(file) {
        command.env("NODE_PATH", modules);
    }
    let output = command
        .output()
        .context("Failed to run node to evaluate the IaC file")?;
    decode_eval_output("node", &output)
}

fn nearest_node_modules(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join("node_modules");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

fn decode_eval_output(runtime: &str, output: &std::process::Output) -> Result<Value> {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        bail!("{runtime} failed to evaluate IaC file.\n{stderr}\n{stdout}");
    }
    serde_json::from_str(&stdout).with_context(|| {
        format!("{runtime} returned non-JSON output.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    })
}
