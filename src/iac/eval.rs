use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config::LinkedProject;

use super::compiler::project_definition_to_graph;
use super::graph::RailwayGraph;
use super::partial::parse_partial_name;

pub struct EvaluatedFile {
    pub file: PathBuf,
    pub graph: RailwayGraph,
    pub partial: Option<String>,
}

/// Linked project + command, matching the JSON the legacy TS runner sent as `context`.
#[derive(Clone, Debug, Default)]
pub struct EvalContext {
    pub command: Option<String>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub environment_id: Option<String>,
    pub environment: Option<String>,
    pub environment_name: Option<String>,
}

impl EvalContext {
    pub fn from_linked_project(linked: &LinkedProject, command: &str) -> Self {
        let environment = linked.environment_name.clone();
        Self {
            command: Some(command.to_string()),
            project_id: Some(linked.project.clone()),
            project_name: linked.name.clone(),
            environment_id: linked.environment.clone(),
            environment: environment.clone(),
            environment_name: environment,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "command": self.command,
            "projectId": self.project_id,
            "projectName": self.project_name,
            "environmentId": self.environment_id,
            "environment": self.environment,
            "environmentName": self.environment_name,
        })
    }
}

pub fn evaluate_file(file: &Path) -> Result<EvaluatedFile> {
    evaluate_file_with_context(file, &EvalContext::default())
}

pub fn evaluate_file_with_context(file: &Path, ctx: &EvalContext) -> Result<EvaluatedFile> {
    let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let context_json = ctx.to_json().to_string();
    let ext = file
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let payload = match ext.as_str() {
        "py" => evaluate_python(&file, &context_json)?,
        "go" => evaluate_go(&file, &context_json)?,
        "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" => evaluate_javascript(&file, &context_json)?,
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

fn with_eval_context(command: &mut Command, context_json: &str) {
    command.env("RAILWAY_IAC_CONTEXT", context_json);
}

fn evaluate_python(file: &Path, context_json: &str) -> Result<Value> {
    let script = r#"
import importlib.util, inspect, json, os, sys
path = sys.argv[1]
payload = json.loads(os.environ.get("RAILWAY_IAC_CONTEXT") or "{}")

def make_ctx(payload):
    try:
        from railway_sdk import create_railway_context
        return create_railway_context(payload)
    except Exception:
        class _Shared:
            def __getattr__(self, name):
                if name.startswith("_"):
                    raise AttributeError(name)
                return {"type": "sharedReference", "name": name}
        class _Ctx(dict):
            def __init__(self, p):
                super().__init__(p)
                env = p.get("environment") or p.get("environmentName")
                self.environment = env
                self.environmentName = env
                self.shared = _Shared()
            def is_environment(self, name):
                return self.environment == name
            def random_string(self, label="random", bytes=12):
                import hashlib
                seed = f"railway-iac:{self.environment or 'default'}:{label}"
                return hashlib.sha256(seed.encode()).hexdigest()[: bytes * 2]
        return _Ctx(payload)

ctx = make_ctx(payload)
spec = importlib.util.spec_from_file_location("railway_sdk_user", path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
partial = getattr(mod, "PARTIAL", None) or getattr(mod, "Partial", None) or getattr(mod, "partial", None)
candidate = getattr(mod, "main", None) or getattr(mod, "Railway", None) or getattr(mod, "default", None)
if callable(candidate):
    try:
        params = list(inspect.signature(candidate).parameters.values())
    except (TypeError, ValueError):
        params = [None]
    project = candidate() if not params else candidate(ctx)
else:
    project = candidate
if hasattr(project, "to_graph"):
    project = project.to_graph()
print(json.dumps({"partial": partial, "project": project}, default=str))
"#;
    let mut command = Command::new("python3");
    command.args(["-c", script, &file.to_string_lossy()]);
    with_eval_context(&mut command, context_json);
    let output = command
        .output()
        .context("Failed to run python3 to evaluate .railway/railway.py")?;
    decode_eval_output("python3", &output)
}

fn evaluate_go(file: &Path, context_json: &str) -> Result<Value> {
    let dir = file
        .parent()
        .context("Go IaC file has no parent directory")?;
    let wrapper = dir.join("railway_iac_eval_main.go");
    let has_partial = fs::read_to_string(file)
        .unwrap_or_default()
        .contains("Partial");
    fs::write(&wrapper, go_eval_wrapper(has_partial))?;
    let mut command = Command::new("go");
    command
        .args([
            "run",
            file.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("railway.go"),
            wrapper
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("railway_iac_eval_main.go"),
        ])
        .current_dir(dir);
    with_eval_context(&mut command, context_json);
    let output = command.output();
    let _ = fs::remove_file(&wrapper);
    decode_eval_output(
        "go run",
        &output.context("Failed to run go to evaluate .railway/railway.go")?,
    )
}

fn go_eval_wrapper(include_partial: bool) -> String {
    let payload = if include_partial {
        r#"map[string]any{"partial": Partial, "project": railwayIacGraph(result)}"#
    } else {
        r#"map[string]any{"project": railwayIacGraph(result)}"#
    };
    format!(
        r#"package main
import (
  "encoding/json"
  "os"
  "reflect"
  "strings"
)

func railwayIacContextValue(fnType reflect.Type) reflect.Value {{
  raw := os.Getenv("RAILWAY_IAC_CONTEXT")
  var payload map[string]any
  _ = json.Unmarshal([]byte(raw), &payload)
  in := fnType.In(0)
  arg := reflect.New(in).Elem()
  railwayIacFillValue(arg, payload)
  return arg
}}

func railwayIacFillValue(v reflect.Value, payload map[string]any) {{
  if v.Kind() == reflect.Pointer {{
    if v.IsNil() {{
      v.Set(reflect.New(v.Type().Elem()))
    }}
    railwayIacFillValue(v.Elem(), payload)
    return
  }}
  if v.Kind() != reflect.Struct {{
    return
  }}
  t := v.Type()
  for i := 0; i < t.NumField(); i++ {{
    field := t.Field(i)
    if !field.IsExported() || field.Type.Kind() != reflect.String {{
      continue
    }}
    name := strings.ToLower(field.Name)
    keys := []string{{field.Name, name}}
    switch name {{
    case "command":
      keys = append(keys, "command")
    case "projectid":
      keys = append(keys, "projectId")
    case "projectname":
      keys = append(keys, "projectName")
    case "environmentid":
      keys = append(keys, "environmentId")
    case "environment":
      keys = append(keys, "environment", "environmentName")
    case "environmentname":
      keys = append(keys, "environmentName", "environment")
    }}
    for _, key := range keys {{
      if text, ok := payload[key].(string); ok && text != "" {{
        v.Field(i).SetString(text)
        break
      }}
    }}
  }}
}}

func railwayIacGraph(project reflect.Value) any {{
  if method := project.MethodByName("Graph"); method.IsValid() {{
    return method.Call(nil)[0].Interface()
  }}
  return project.Interface()
}}

func main() {{
  fn := reflect.ValueOf(Railway)
  var result reflect.Value
  if fn.Type().NumIn() == 0 {{
    result = fn.Call(nil)[0]
  }} else {{
    result = fn.Call([]reflect.Value{{railwayIacContextValue(fn.Type())}})[0]
  }}
  out, err := json.Marshal({payload})
  if err != nil {{ panic(err) }}
  os.Stdout.Write(out)
}}
"#
    )
}

fn evaluate_javascript(file: &Path, context_json: &str) -> Result<Value> {
    let script = r#"
import { createHash } from "node:crypto";
import { pathToFileURL } from "node:url";
const file = process.argv[1];
const input = JSON.parse(process.env.RAILWAY_IAC_CONTEXT || "{}");
const environment = input.environment ?? input.environmentName ?? undefined;
function fallbackContext(payload) {
  return {
    ...payload,
    ...(environment ? { environment, environmentName: environment } : {}),
    isEnvironment: (name) => environment === name,
    randomString: (label = "random", bytes = 12) =>
      createHash("sha256")
        .update(`railway-iac:${environment ?? "default"}:${label}`)
        .digest("hex")
        .slice(0, bytes * 2),
    shared: new Proxy({}, {
      get: (_target, name) => {
        if (typeof name !== "string" || name.startsWith("_")) return undefined;
        return { type: "sharedReference", name };
      },
    }),
  };
}
let ctx = fallbackContext(input);
try {
  const iac = await import("railway/iac");
  if (typeof iac.createRailwayContext === "function") {
    ctx = iac.createRailwayContext(input);
  }
} catch {}
const mod = await import(`${pathToFileURL(file).href}?t=${Date.now()}`);
const partial = mod.partial ?? mod.PARTIAL ?? mod.Partial ?? undefined;
let exported = mod.default ?? mod.main ?? mod.Railway ?? mod;
while (exported && typeof exported === "object" && "default" in exported && exported.name == null && exported.resources == null) {
  exported = exported.default;
}
const projectFactory = (name, definition = {}) => {
  const resources = (definition.resources ?? definition.services ?? []).flat?.() ?? definition.resources ?? [];
  return { name, ...definition, resources };
};
const project = typeof exported === "function" ? await exported(ctx, projectFactory) : exported;
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
    with_eval_context(&mut command, context_json);
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
