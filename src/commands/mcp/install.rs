use super::*;
use crate::commands::skills::resolve_tools;
use serde_json::{Value as JsonValue, json};
use std::path::{Path, PathBuf};

const REMOTE_MCP_URL: &str = "https://mcp.railway.com";

/// Install the Railway MCP server config into AI coding tools (Claude Code, Cursor, OpenAI Codex, OpenCode, GitHub Copilot, Factory Droid).
///
/// Merges a `railway` server entry into each tool's MCP config file. Without `--agent`, only configures detected tools (those with their config dir present).
#[derive(Parser)]
pub struct Args {
    /// Target specific agent(s) instead of all detected (e.g. --agent cursor)
    #[clap(long)]
    agent: Vec<String>,

    /// Configure the remote MCP server via the CLI proxy (`railway mcp proxy` →
    /// mcp.railway.com, auth via `railway login`). This is the default; the flag is
    /// kept as a compatibility alias. Ignored when `--oauth` is set.
    #[clap(long, conflicts_with = "local")]
    remote: bool,

    /// Configure the local GraphQL-backed MCP server (`railway mcp local`) instead of
    /// the remote CLI proxy default.
    #[clap(long, conflicts_with_all = ["remote", "oauth"])]
    local: bool,

    /// Use editor OAuth against https://mcp.railway.com directly (not the CLI proxy).
    /// Takes precedence over `--remote`.
    #[clap(long, conflicts_with = "local")]
    oauth: bool,
}

/// Which flavor of the Railway MCP server an install writes into a harness config.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum McpTransport {
    /// `railway mcp local` — in-process stdio server backed by GraphQL.
    Local,
    /// `railway mcp proxy` — stdio bridge to mcp.railway.com that
    /// authenticates with the CLI's stored login. Also what a bare
    /// `railway mcp` starts.
    RemoteProxy,
    /// Plain mcp.railway.com URL — the editor authenticates via its own OAuth flow.
    RemoteOauth,
}

impl McpTransport {
    /// Resolve the install transport from CLI flags.
    ///
    /// Default is the remote CLI proxy. `--local` selects the in-process server;
    /// `--oauth` selects plain `https://mcp.railway.com`. `--remote` remains
    /// accepted as an explicit alias of the default.
    pub(crate) fn from_flags(remote: bool, oauth: bool, local: bool) -> Self {
        match (local, oauth, remote) {
            (true, _, _) => Self::Local,
            (false, true, _) => Self::RemoteOauth,
            (false, false, _) => Self::RemoteProxy,
        }
    }
}

/// The argv written into harness configs for the stdio transports.
fn stdio_args(transport: McpTransport) -> Vec<&'static str> {
    match transport {
        McpTransport::Local => vec!["mcp", "local"],
        // Written explicitly rather than as a bare `mcp`, even though that is
        // now the default: an installed config should not change meaning if
        // the default ever moves again.
        McpTransport::RemoteProxy => vec!["mcp", "proxy"],
        // RemoteOauth entries are URL-based; callers never ask for its argv.
        McpTransport::RemoteOauth => unreachable!("RemoteOauth has no stdio argv"),
    }
}

pub async fn command(args: Args) -> Result<()> {
    if args.oauth && args.remote {
        eprintln!(
            "{} {}",
            "!".yellow().bold(),
            "Using editor OAuth (https://mcp.railway.com); --remote is ignored with --oauth."
                .yellow()
        );
    }
    install_mcp(
        &args.agent,
        McpTransport::from_flags(args.remote, args.oauth, args.local),
        false,
    )
    .await
}

// `quiet` suppresses the section header, the "Installing … to:" line, per-tool
// success lines, and the footer/restart notice — used by the embedded
// agent-setup flow, which prints its own collapsed one-line summary. Failures
// are always surfaced so a silent error can't contradict that summary.
pub(crate) async fn install_mcp(
    agent_filter: &[String],
    transport: McpTransport,
    quiet: bool,
) -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let tools = resolve_tools(&home, agent_filter)?;

    if !quiet {
        println!("\n{}\n", "Railway MCP".bold());
    }

    let configurable: Vec<_> = tools
        .iter()
        .filter(|t| supports_mcp(t.slug))
        .cloned()
        .collect();

    if configurable.is_empty() {
        // The skills command auto-includes "universal", which has no MCP target.
        // Tell the user nothing was configured rather than silently no-op.
        if !quiet {
            println!("{}", "No MCP-capable tools selected or detected.".yellow());
            if tools.iter().any(|t| t.slug == "universal") {
                println!(
                    "{} The universal `.agents` directory has no MCP convention; pass --agent to target a specific tool.",
                    "!".yellow().bold()
                );
            }
        }
        return Ok(());
    }

    let names: Vec<_> = configurable.iter().map(|t| t.name).collect();
    let transport_desc = match transport {
        McpTransport::Local => "local stdio".to_string(),
        McpTransport::RemoteProxy => {
            format!("remote ({REMOTE_MCP_URL} via CLI proxy, uses `railway login`)")
        }
        McpTransport::RemoteOauth => format!("remote ({REMOTE_MCP_URL}, editor OAuth)"),
    };
    if !quiet {
        println!(
            "{} {} {} {}\n",
            "Installing".bold(),
            transport_desc.cyan(),
            "to:".bold(),
            names.join(", ")
        );
    }

    for tool in &configurable {
        let path = config_path(tool.slug, &home);
        match install_for(tool.slug, &path, transport) {
            Ok(()) => {
                if !quiet {
                    println!(
                        "{} {}: configured \u{2192} {}",
                        "\u{2713}".green(),
                        tool.name.bold(),
                        path.display().to_string().cyan()
                    );
                }
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

    if !quiet {
        println!("\n{}", "MCP server installed successfully!".green().bold());
        println!(
            "{} You may need to restart your tool(s) for the MCP server to register.\n",
            "!".yellow().bold()
        );
    }

    Ok(())
}

pub(crate) fn supports_mcp(slug: &str) -> bool {
    matches!(
        slug,
        "claude-code" | "cursor" | "opencode" | "codex" | "copilot" | "factory-droid"
    )
}

fn config_path(slug: &str, home: &Path) -> PathBuf {
    match slug {
        "claude-code" => home.join(".claude.json"),
        "cursor" => home.join(".cursor").join("mcp.json"),
        "opencode" => home.join(".config").join("opencode").join("opencode.json"),
        "codex" => home.join(".codex").join("config.toml"),
        "copilot" => home.join(".copilot").join("mcp-config.json"),
        "factory-droid" => home.join(".factory").join("mcp.json"),
        // supports_mcp gates this; unreachable in practice.
        _ => home.join(".unsupported"),
    }
}

const ALL_TRANSPORTS: [McpTransport; 3] = [
    McpTransport::Local,
    McpTransport::RemoteProxy,
    McpTransport::RemoteOauth,
];

/// True when a railway MCP entry is configured for any transport — the
/// help health check doesn't care which one `setup agent` installed. Reads
/// each config file once instead of once per transport.
pub(crate) fn mcp_configured_any_transport(home: &Path, slug: &str) -> bool {
    let path = config_path(slug, home);
    match slug {
        "claude-code" | "cursor" | "copilot" | "factory-droid" => read_json_or_empty(&path)
            .ok()
            .and_then(|root| root.pointer("/mcpServers/railway").cloned())
            .is_some_and(|entry| {
                ALL_TRANSPORTS
                    .iter()
                    .any(|t| json_mcp_entry_matches(&entry, *t))
            }),
        "opencode" => read_json_or_empty(&path)
            .ok()
            .and_then(|root| root.pointer("/mcp/railway").cloned())
            .is_some_and(|entry| {
                ALL_TRANSPORTS
                    .iter()
                    .any(|t| opencode_mcp_entry_matches(&entry, *t))
            }),
        // Codex keeps its TOML matching in one place at the cost of extra
        // reads for this one tool.
        "codex" => ALL_TRANSPORTS
            .iter()
            .any(|t| codex_mcp_configured(&path, *t)),
        _ => false,
    }
}

pub(crate) fn mcp_configured_for_slug(home: &Path, slug: &str, transport: McpTransport) -> bool {
    let path = config_path(slug, home);

    match slug {
        "claude-code" | "cursor" | "copilot" | "factory-droid" => read_json_or_empty(&path)
            .ok()
            .and_then(|root| root.pointer("/mcpServers/railway").cloned())
            .is_some_and(|entry| json_mcp_entry_matches(&entry, transport)),
        "opencode" => read_json_or_empty(&path)
            .ok()
            .and_then(|root| root.pointer("/mcp/railway").cloned())
            .is_some_and(|entry| opencode_mcp_entry_matches(&entry, transport)),
        "codex" => codex_mcp_configured(&path, transport),
        _ => false,
    }
}

/// Classify an installed `railway mcp …` entry by which server it starts.
///
/// Three-way since the cutover. `mcp local` is the in-process server; `mcp
/// proxy` is the remote one; and a bare `mcp` — what installs written before
/// the cutover contain — now starts the proxy too, so it classifies as remote
/// rather than as the local install it used to be.
fn stdio_argv_matches<'a>(
    args: impl Iterator<Item = &'a str> + Clone,
    transport: McpTransport,
) -> bool {
    if !args.clone().any(|a| a == "mcp") {
        return false;
    }
    let has_local = args.clone().any(|a| a == "local");
    match transport {
        McpTransport::Local => has_local,
        McpTransport::RemoteProxy => !has_local,
        // URL-based; callers match it before narrowing to a stdio transport.
        McpTransport::RemoteOauth => false,
    }
}

fn json_mcp_entry_matches(entry: &JsonValue, transport: McpTransport) -> bool {
    match transport {
        McpTransport::RemoteOauth => {
            entry.get("url").and_then(JsonValue::as_str) == Some(REMOTE_MCP_URL)
        }
        stdio => {
            entry.get("command").and_then(JsonValue::as_str) == Some("railway")
                && entry
                    .get("args")
                    .and_then(JsonValue::as_array)
                    .is_some_and(|args| {
                        stdio_argv_matches(args.iter().filter_map(JsonValue::as_str), stdio)
                    })
        }
    }
}

fn opencode_mcp_entry_matches(entry: &JsonValue, transport: McpTransport) -> bool {
    match transport {
        McpTransport::RemoteOauth => {
            entry.get("type").and_then(JsonValue::as_str) == Some("remote")
                && entry.get("url").and_then(JsonValue::as_str) == Some(REMOTE_MCP_URL)
        }
        stdio => {
            entry.get("type").and_then(JsonValue::as_str) == Some("local")
                && entry
                    .get("command")
                    .and_then(JsonValue::as_array)
                    .is_some_and(|command| {
                        command.first().and_then(JsonValue::as_str) == Some("railway")
                            && stdio_argv_matches(
                                command.iter().skip(1).filter_map(JsonValue::as_str),
                                stdio,
                            )
                    })
        }
    }
}

fn codex_mcp_configured(path: &Path, transport: McpTransport) -> bool {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = existing.parse::<toml::Value>() else {
        return false;
    };

    doc.get("mcp_servers")
        .and_then(|servers| servers.get("railway"))
        .is_some_and(|entry| match transport {
            McpTransport::RemoteOauth => {
                entry.get("url").and_then(toml::Value::as_str) == Some(REMOTE_MCP_URL)
            }
            stdio => {
                entry.get("command").and_then(toml::Value::as_str) == Some("railway")
                    && entry
                        .get("args")
                        .and_then(toml::Value::as_array)
                        .is_some_and(|args| {
                            stdio_argv_matches(args.iter().filter_map(toml::Value::as_str), stdio)
                        })
            }
        })
}

fn install_for(slug: &str, path: &Path, transport: McpTransport) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    match slug {
        "claude-code" => {
            let entry = match transport {
                McpTransport::RemoteOauth => json!({ "type": "http", "url": REMOTE_MCP_URL }),
                stdio => json!({ "command": "railway", "args": stdio_args(stdio) }),
            };
            write_json_mcp_servers(path, entry)
        }
        "cursor" => {
            // Cursor auto-detects HTTP/SSE from the presence of `url`.
            let entry = match transport {
                McpTransport::RemoteOauth => json!({ "url": REMOTE_MCP_URL }),
                stdio => json!({ "command": "railway", "args": stdio_args(stdio) }),
            };
            write_json_mcp_servers(path, entry)
        }
        "opencode" => write_opencode_mcp(path, transport),
        "codex" => write_codex_toml(path, transport),
        "copilot" => {
            let entry = match transport {
                McpTransport::RemoteOauth => {
                    json!({ "type": "http", "url": REMOTE_MCP_URL, "tools": ["*"] })
                }
                stdio => json!({
                    "type": "local",
                    "command": "railway",
                    "args": stdio_args(stdio),
                    "tools": ["*"]
                }),
            };
            write_json_mcp_servers(path, entry)
        }
        "factory-droid" => {
            let entry = match transport {
                McpTransport::RemoteOauth => {
                    json!({ "type": "http", "url": REMOTE_MCP_URL, "disabled": false })
                }
                stdio => json!({
                    "type": "stdio",
                    "command": "railway",
                    "args": stdio_args(stdio),
                    "disabled": false
                }),
            };
            write_json_mcp_servers(path, entry)
        }
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
fn write_opencode_mcp(path: &Path, transport: McpTransport) -> Result<()> {
    let mut root = read_json_or_empty(path)?;
    let entry = match transport {
        McpTransport::RemoteOauth => json!({
            "type": "remote",
            "url": REMOTE_MCP_URL,
            "enabled": true,
        }),
        stdio => {
            let mut command = vec!["railway"];
            command.extend(stdio_args(stdio));
            json!({
                "type": "local",
                "command": command,
                "enabled": true,
            })
        }
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

fn write_codex_toml(path: &Path, transport: McpTransport) -> Result<()> {
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
    match transport {
        McpTransport::RemoteOauth => {
            railway.insert(
                "url".to_string(),
                toml::Value::String(REMOTE_MCP_URL.to_string()),
            );
        }
        stdio => {
            railway.insert(
                "command".to_string(),
                toml::Value::String("railway".to_string()),
            );
            railway.insert(
                "args".to_string(),
                toml::Value::Array(
                    stdio_args(stdio)
                        .into_iter()
                        .map(|a| toml::Value::String(a.to_string()))
                        .collect(),
                ),
            );
        }
    }
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
    fn detects_existing_cursor_bare_mcp_as_remote() {
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

        // A bare `mcp` argv started the in-process server before the
        // cutover and starts the proxy after it, so an untouched
        // pre-cutover config now reads as a remote install.
        assert!(mcp_configured_for_slug(
            home.path(),
            "cursor",
            McpTransport::RemoteProxy
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "cursor",
            McpTransport::Local
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "cursor",
            McpTransport::RemoteOauth
        ));
    }

    #[test]
    fn local_installs_are_written_explicitly() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".cursor").join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        install_for("cursor", &path, McpTransport::Local).unwrap();

        let root: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // Must be `mcp local`, not a bare `mcp` — that starts the proxy now.
        assert_eq!(
            root.pointer("/mcpServers/railway/args").unwrap(),
            &serde_json::json!(["mcp", "local"])
        );

        assert!(mcp_configured_for_slug(
            home.path(),
            "cursor",
            McpTransport::Local
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "cursor",
            McpTransport::RemoteProxy
        ));
    }

    #[test]
    fn argv_classification_is_three_way() {
        let cases: [(&[&str], McpTransport, bool); 6] = [
            (&["mcp", "local"], McpTransport::Local, true),
            (&["mcp", "local"], McpTransport::RemoteProxy, false),
            (&["mcp", "proxy"], McpTransport::RemoteProxy, true),
            (&["mcp", "proxy"], McpTransport::Local, false),
            // Pre-cutover config: bare `mcp` starts the proxy now.
            (&["mcp"], McpTransport::RemoteProxy, true),
            (&["mcp"], McpTransport::Local, false),
        ];
        for (argv, transport, expected) in cases {
            assert_eq!(
                stdio_argv_matches(argv.iter().copied(), transport),
                expected,
                "{argv:?} against {transport:?}"
            );
        }
    }

    #[test]
    fn proxy_entry_is_not_mistaken_for_local() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".cursor").join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        install_for("cursor", &path, McpTransport::RemoteProxy).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let root: JsonValue = serde_json::from_str(&written).unwrap();
        let railway = root.pointer("/mcpServers/railway").unwrap();
        assert_eq!(
            railway.get("args").unwrap(),
            &serde_json::json!(["mcp", "proxy"])
        );

        assert!(mcp_configured_for_slug(
            home.path(),
            "cursor",
            McpTransport::RemoteProxy
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "cursor",
            McpTransport::Local
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "cursor",
            McpTransport::RemoteOauth
        ));
        assert!(mcp_configured_any_transport(home.path(), "cursor"));
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

        assert!(mcp_configured_for_slug(
            home.path(),
            "opencode",
            McpTransport::RemoteOauth
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "opencode",
            McpTransport::Local
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "opencode",
            McpTransport::RemoteProxy
        ));
    }

    #[test]
    fn writes_and_detects_opencode_proxy_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home
            .path()
            .join(".config")
            .join("opencode")
            .join("opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        install_for("opencode", &path, McpTransport::RemoteProxy).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let root: JsonValue = serde_json::from_str(&written).unwrap();
        let railway = root.pointer("/mcp/railway").unwrap();
        assert_eq!(
            railway.get("command").unwrap(),
            &serde_json::json!(["railway", "mcp", "proxy"])
        );

        assert!(mcp_configured_for_slug(
            home.path(),
            "opencode",
            McpTransport::RemoteProxy
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "opencode",
            McpTransport::Local
        ));
    }

    #[test]
    fn detects_existing_codex_bare_mcp_as_remote() {
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

        // A bare `mcp` argv started the in-process server before the
        // cutover and starts the proxy after it, so an untouched
        // pre-cutover config now reads as a remote install.
        assert!(mcp_configured_for_slug(
            home.path(),
            "codex",
            McpTransport::RemoteProxy
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "codex",
            McpTransport::Local
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "codex",
            McpTransport::RemoteOauth
        ));
    }

    #[test]
    fn writes_and_detects_codex_proxy_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        write_codex_toml(&path, McpTransport::RemoteProxy).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let doc = written.parse::<toml::Value>().unwrap();
        let args = doc
            .get("mcp_servers")
            .and_then(|servers| servers.get("railway"))
            .and_then(|railway| railway.get("args"))
            .and_then(toml::Value::as_array)
            .unwrap();
        assert_eq!(args.len(), 2);
        assert_eq!(args[1].as_str(), Some("proxy"));

        assert!(mcp_configured_for_slug(
            home.path(),
            "codex",
            McpTransport::RemoteProxy
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "codex",
            McpTransport::Local
        ));
    }

    #[test]
    fn detects_existing_codex_remote_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"
                [mcp_servers.railway]
                url = "https://mcp.railway.com"
            "#,
        )
        .unwrap();

        assert!(mcp_configured_for_slug(
            home.path(),
            "codex",
            McpTransport::RemoteOauth
        ));
        assert!(!mcp_configured_for_slug(
            home.path(),
            "codex",
            McpTransport::Local
        ));
    }

    #[test]
    fn writes_codex_remote_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        write_codex_toml(&path, McpTransport::RemoteOauth).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let doc = written.parse::<toml::Value>().unwrap();
        let railway = doc
            .get("mcp_servers")
            .and_then(|servers| servers.get("railway"))
            .unwrap();

        assert_eq!(
            railway.get("url").and_then(toml::Value::as_str),
            Some("https://mcp.railway.com")
        );
        assert!(railway.get("command").is_none());
        assert!(railway.get("args").is_none());
    }

    #[test]
    fn writes_copilot_local_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".copilot").join("mcp-config.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        install_for("copilot", &path, McpTransport::Local).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let root: JsonValue = serde_json::from_str(&written).unwrap();
        let railway = root.pointer("/mcpServers/railway").unwrap();

        assert_eq!(
            railway.get("type").and_then(JsonValue::as_str),
            Some("local")
        );
        assert_eq!(
            railway.get("command").and_then(JsonValue::as_str),
            Some("railway")
        );
        assert!(mcp_configured_for_slug(
            home.path(),
            "copilot",
            McpTransport::Local
        ));
    }

    #[test]
    fn writes_factory_droid_remote_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".factory").join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        install_for("factory-droid", &path, McpTransport::RemoteOauth).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let root: JsonValue = serde_json::from_str(&written).unwrap();
        let railway = root.pointer("/mcpServers/railway").unwrap();

        assert_eq!(
            railway.get("type").and_then(JsonValue::as_str),
            Some("http")
        );
        assert_eq!(
            railway.get("url").and_then(JsonValue::as_str),
            Some("https://mcp.railway.com")
        );
        assert!(mcp_configured_for_slug(
            home.path(),
            "factory-droid",
            McpTransport::RemoteOauth
        ));
    }

    #[test]
    fn from_flags_defaults_to_remote_proxy() {
        assert_eq!(
            McpTransport::from_flags(false, false, false),
            McpTransport::RemoteProxy
        );
        // `--remote` remains an explicit alias of the default.
        assert_eq!(
            McpTransport::from_flags(true, false, false),
            McpTransport::RemoteProxy
        );
        assert_eq!(
            McpTransport::from_flags(false, false, true),
            McpTransport::Local
        );
        assert_eq!(
            McpTransport::from_flags(false, true, false),
            McpTransport::RemoteOauth
        );
        // `--local` wins over a stale `--remote` if both somehow appear.
        assert_eq!(
            McpTransport::from_flags(true, false, true),
            McpTransport::Local
        );
    }
}
