use super::*;
use crate::commands::skills::resolve_tools;
use serde_json::{Value as JsonValue, json};
use std::path::{Path, PathBuf};

const REMOTE_MCP_URL: &str = "https://mcp.railway.com";

/// Install the Railway MCP server config into AI coding tools (Claude Code, Cursor, OpenAI Codex, OpenCode).
///
/// Merges a `railway` server entry into each tool's MCP config file. Without `--agent`, only configures detected tools (those with their config dir present).
#[derive(Parser)]
pub struct Args {
    /// Target specific agent(s) instead of all detected (e.g. --agent cursor)
    #[clap(long)]
    agent: Vec<String>,

    /// Configure the remote HTTP MCP server at mcp.railway.com instead of the local stdio server.
    /// Codex is skipped because it only supports stdio MCP servers.
    #[clap(long)]
    remote: bool,
}

pub async fn command(args: Args) -> Result<()> {
    install_mcp(&args.agent, args.remote).await
}

pub(crate) async fn install_mcp(agent_filter: &[String], remote: bool) -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let tools = resolve_tools(&home, agent_filter)?;

    println!("\n{}\n", "Railway MCP".bold());

    let (configurable, skipped_remote): (Vec<_>, Vec<_>) = tools
        .iter()
        .filter(|t| supports_mcp(t.slug))
        .cloned()
        .partition(|t| !remote || supports_remote(t.slug));

    for tool in &skipped_remote {
        println!(
            "{} {}: skipped \u{2192} only stdio MCP is supported (no remote HTTP transport)",
            "-".dimmed(),
            tool.name.bold()
        );
    }

    if configurable.is_empty() {
        // The skills command auto-includes "universal", which has no MCP target.
        // Tell the user nothing was configured rather than silently no-op.
        println!("{}", "No MCP-capable tools selected or detected.".yellow());
        if tools.iter().any(|t| t.slug == "universal") {
            println!(
                "{} The universal `.agents` directory has no MCP convention; pass --agent to target a specific tool.",
                "!".yellow().bold()
            );
        }
        return Ok(());
    }

    let names: Vec<_> = configurable.iter().map(|t| t.name).collect();
    let transport = if remote {
        format!("remote ({})", REMOTE_MCP_URL).cyan()
    } else {
        "local stdio".cyan()
    };
    println!(
        "{} {} {} {}\n",
        "Installing".bold(),
        transport,
        "to:".bold(),
        names.join(", ")
    );

    for tool in &configurable {
        let path = config_path(tool.slug, &home);
        match install_for(tool.slug, &path, remote) {
            Ok(()) => {
                println!(
                    "{} {}: configured \u{2192} {}",
                    "\u{2713}".green(),
                    tool.name.bold(),
                    path.display().to_string().cyan()
                );
            }
            Err(e) => {
                println!(
                    "{} {}: failed \u{2192} {}",
                    "\u{2717}".red(),
                    tool.name.bold(),
                    e.to_string().red()
                );
            }
        }
    }

    println!("\n{}", "MCP server installed successfully!".green().bold());
    println!(
        "{} You may need to restart your tool(s) for the MCP server to register.\n",
        "!".yellow().bold()
    );

    Ok(())
}

fn supports_mcp(slug: &str) -> bool {
    matches!(slug, "claude-code" | "cursor" | "opencode" | "codex")
}

/// Whether the harness can talk to a remote HTTP MCP server. Codex only speaks
/// stdio today, so it's excluded from `--remote` runs.
fn supports_remote(slug: &str) -> bool {
    matches!(slug, "claude-code" | "cursor" | "opencode")
}

fn config_path(slug: &str, home: &Path) -> PathBuf {
    match slug {
        "claude-code" => home.join(".claude.json"),
        "cursor" => home.join(".cursor").join("mcp.json"),
        "opencode" => home.join(".config").join("opencode").join("opencode.json"),
        "codex" => home.join(".codex").join("config.toml"),
        // supports_mcp gates this; unreachable in practice.
        _ => home.join(".unsupported"),
    }
}

pub(crate) fn mcp_configured_for_slug(home: &Path, slug: &str, remote: bool) -> bool {
    let path = config_path(slug, home);

    match slug {
        "claude-code" | "cursor" => read_json_or_empty(&path)
            .ok()
            .and_then(|root| root.pointer("/mcpServers/railway").cloned())
            .is_some_and(|entry| json_mcp_entry_matches(&entry, remote)),
        "opencode" => read_json_or_empty(&path)
            .ok()
            .and_then(|root| root.pointer("/mcp/railway").cloned())
            .is_some_and(|entry| opencode_mcp_entry_matches(&entry, remote)),
        "codex" if !remote => codex_mcp_configured(&path),
        _ => false,
    }
}

fn json_mcp_entry_matches(entry: &JsonValue, remote: bool) -> bool {
    if remote {
        entry.get("url").and_then(JsonValue::as_str) == Some(REMOTE_MCP_URL)
    } else {
        entry.get("command").and_then(JsonValue::as_str) == Some("railway")
            && entry
                .get("args")
                .and_then(JsonValue::as_array)
                .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("mcp")))
    }
}

fn opencode_mcp_entry_matches(entry: &JsonValue, remote: bool) -> bool {
    if remote {
        entry.get("type").and_then(JsonValue::as_str) == Some("remote")
            && entry.get("url").and_then(JsonValue::as_str) == Some(REMOTE_MCP_URL)
    } else {
        entry.get("type").and_then(JsonValue::as_str) == Some("local")
            && entry
                .get("command")
                .and_then(JsonValue::as_array)
                .is_some_and(|command| {
                    command.first().and_then(JsonValue::as_str) == Some("railway")
                        && command.iter().any(|arg| arg.as_str() == Some("mcp"))
                })
    }
}

fn codex_mcp_configured(path: &Path) -> bool {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = existing.parse::<toml::Value>() else {
        return false;
    };

    doc.get("mcp_servers")
        .and_then(|servers| servers.get("railway"))
        .is_some_and(|entry| {
            entry.get("command").and_then(toml::Value::as_str) == Some("railway")
                && entry
                    .get("args")
                    .and_then(toml::Value::as_array)
                    .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("mcp")))
        })
}

fn install_for(slug: &str, path: &Path, remote: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    match slug {
        "claude-code" => {
            let entry = if remote {
                json!({ "type": "http", "url": REMOTE_MCP_URL })
            } else {
                json!({ "command": "railway", "args": ["mcp"] })
            };
            write_json_mcp_servers(path, entry)
        }
        "cursor" => {
            // Cursor auto-detects HTTP/SSE from the presence of `url`.
            let entry = if remote {
                json!({ "url": REMOTE_MCP_URL })
            } else {
                json!({ "command": "railway", "args": ["mcp"] })
            };
            write_json_mcp_servers(path, entry)
        }
        "opencode" => write_opencode_mcp(path, remote),
        // supports_remote() filters codex out of remote runs before reaching here.
        "codex" => write_codex_toml(path),
        _ => bail!("Unsupported MCP target: {}", slug),
    }
}

/// Read existing JSON (if any), set `mcpServers.railway = entry`, write back.
fn write_json_mcp_servers(path: &Path, entry: JsonValue) -> Result<()> {
    let mut root = read_json_or_empty(path)?;

    let obj = root
        .as_object_mut()
        .context("Existing config root is not a JSON object")?;
    let servers = obj
        .entry("mcpServers".to_string())
        .or_insert_with(|| JsonValue::Object(Default::default()));
    let servers = servers
        .as_object_mut()
        .context("`mcpServers` is not a JSON object")?;
    servers.insert("railway".to_string(), entry);

    write_json_pretty(path, &root)
}

/// OpenCode uses an `mcp` key with a slightly different per-server schema
/// (`type: "local"` with `command` as an argv array, or `type: "remote"` with
/// `url`). See docs.opencode.ai for the canonical shape.
fn write_opencode_mcp(path: &Path, remote: bool) -> Result<()> {
    let mut root = read_json_or_empty(path)?;
    let entry = if remote {
        json!({
            "type": "remote",
            "url": REMOTE_MCP_URL,
            "enabled": true,
        })
    } else {
        json!({
            "type": "local",
            "command": ["railway", "mcp"],
            "enabled": true,
        })
    };

    let obj = root
        .as_object_mut()
        .context("Existing config root is not a JSON object")?;
    let servers = obj
        .entry("mcp".to_string())
        .or_insert_with(|| JsonValue::Object(Default::default()));
    let servers = servers
        .as_object_mut()
        .context("`mcp` is not a JSON object")?;
    servers.insert("railway".to_string(), entry);

    // OpenCode expects a `$schema` for IDE autocomplete; leave existing one if
    // present, set a default if missing.
    if !obj.contains_key("$schema") {
        obj.insert(
            "$schema".to_string(),
            JsonValue::String("https://opencode.ai/config.json".to_string()),
        );
    }

    write_json_pretty(path, &root)
}

fn write_codex_toml(path: &Path) -> Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
    };

    let mut doc: toml::Value = if existing.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        existing
            .parse::<toml::Value>()
            .with_context(|| format!("Failed to parse existing TOML at {}", path.display()))?
    };

    let table = doc
        .as_table_mut()
        .context("Existing config root is not a TOML table")?;

    let servers = table
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let servers = servers
        .as_table_mut()
        .context("`mcp_servers` is not a TOML table")?;

    let mut railway = toml::value::Table::new();
    railway.insert(
        "command".to_string(),
        toml::Value::String("railway".to_string()),
    );
    railway.insert(
        "args".to_string(),
        toml::Value::Array(vec![toml::Value::String("mcp".to_string())]),
    );
    servers.insert("railway".to_string(), toml::Value::Table(railway));

    let serialized = toml::to_string_pretty(&doc).context("Failed to serialize TOML")?;
    crate::util::write_atomic(path, &serialized)
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn read_json_or_empty(path: &Path) -> Result<JsonValue> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(JsonValue::Object(Default::default())),
        // Try strict JSON first; fall back to JSONC (comments + trailing
        // commas) since OpenCode and a few other tools accept JSONC.
        // We always write back strict JSON, so this only loosens the read.
        Ok(s) => match serde_json::from_str(&s) {
            Ok(v) => Ok(v),
            Err(_) => serde_json::from_str(&strip_jsonc(&s))
                .with_context(|| format!("Failed to parse existing JSON at {}", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(JsonValue::Object(Default::default()))
        }
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

/// Strip line/block comments and trailing commas, ignoring anything inside string literals.
fn strip_jsonc(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            out.push(c as char);
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'/' => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'*' => {
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        i += 1;
                    }
                    i = (i + 2).min(bytes.len());
                    continue;
                }
                _ => {}
            }
        }
        if c == b',' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue;
            }
        }
        out.push(c as char);
        i += 1;
    }
    out
}

fn write_json_pretty(path: &Path, value: &JsonValue) -> Result<()> {
    let s = serde_json::to_string_pretty(value).context("Failed to serialize JSON")?;
    crate::util::write_atomic(path, &s)
        .with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_existing_cursor_local_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".cursor").join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
                // Existing user config may be JSONC.
                "mcpServers": {
                    "railway": { "command": "railway", "args": ["mcp"] },
                }
            }"#,
        )
        .unwrap();

        assert!(mcp_configured_for_slug(home.path(), "cursor", false));
        assert!(!mcp_configured_for_slug(home.path(), "cursor", true));
    }

    #[test]
    fn detects_existing_opencode_remote_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home
            .path()
            .join(".config")
            .join("opencode")
            .join("opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
                "mcp": {
                    "railway": {
                        "type": "remote",
                        "url": "https://mcp.railway.com",
                        "enabled": true
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(mcp_configured_for_slug(home.path(), "opencode", true));
        assert!(!mcp_configured_for_slug(home.path(), "opencode", false));
    }

    #[test]
    fn detects_existing_codex_local_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"
                [mcp_servers.railway]
                command = "railway"
                args = ["mcp"]
            "#,
        )
        .unwrap();

        assert!(mcp_configured_for_slug(home.path(), "codex", false));
        assert!(!mcp_configured_for_slug(home.path(), "codex", true));
    }
}
