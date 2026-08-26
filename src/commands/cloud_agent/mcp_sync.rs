//! Syncing a project's MCP servers onto a cloud agent.
//!
//! The source is the launch directory's `.mcp.json` — the project-scope file
//! Claude Code already reads, found by walking up from the cwd to the git
//! root. A repo that gives its people MCP servers by default (mono's
//! `.mcp.json` is the motivating case) should give its cloud agents the same
//! ones: launch from inside the repo and they come along.
//!
//! What ships is the `mcpServers` object, minus entries marked
//! `"disabled": true` and minus the names express-agent provisions itself
//! (`railway`, `playwright`, `railway-machine`) — those are the platform's to
//! own and reconcile. Every shipped name is prefixed `user-` (`buildkite`
//! lands as `user-buildkite`), so an import can never collide with a server
//! already on the agent — the platform's, or one the user added by hand —
//! and everything ours is recognizable at a glance. The rest lands on the VM
//! merged ADD-ONLY into the two JSON dialects the harnesses there read:
//!
//! - `~/.claude.json` `mcpServers` (object form) — claude's user scope, which
//!   asks no per-project approval question the way a repo `.mcp.json` would.
//! - `~/.claude/settings.json` `mcp_servers` (array form) — what the railway
//!   harness reads; entries carry `{name, transport, url|command}`.
//!
//! Add-only means an existing name always wins, whether the user put it there
//! by hand or express-agent's boot reconcile did — the same rule skills sync
//! follows, for the same reason: this path must never overwrite anything on
//! the agent. Codex and grok keep their MCP config in TOML, which a shell
//! merge cannot edit safely; they are deliberately out of scope here and get
//! their servers when the platform's workspace-MCP feature lands.
//!
//! The merge runs under jq, which cloud-agent-base bakes. Every failure path
//! degrades instead of aborting the launch — a missing jq or an unparseable
//! config costs the user their MCP servers, not their session — and a hash
//! recorded on the agent (reported by the main provision script, like
//! `SKILLS-HASH`) keeps an unchanged set from costing an upload.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::prefs::AgentPrefs;

/// Server names express-agent registers on every agent itself. Shipping a
/// local copy could only fight the platform's reconcile.
const RESERVED_NAMES: &[&str] = &["railway", "playwright", "railway-machine"];

/// Every imported name leads with this, so what the CLI brought can never
/// collide with a server already on the agent and is recognizable as an
/// import wherever it shows up.
const NAME_PREFIX: &str = "user-";

/// Payload ceiling. An `.mcp.json` is a page of config; anything near this is
/// not one, and it rides ssh stdin where every retry re-sends it.
const MAX_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub struct PackedMcp {
    /// The filtered `mcpServers` object, serialized deterministically.
    pub payload: Vec<u8>,
    /// Content hash, compared against the copy recorded on the agent so an
    /// unchanged set costs no upload.
    pub hash: String,
    pub names: Vec<String>,
    pub source_path: PathBuf,
}

/// The `.mcp.json` a launch from `dir` should read: the nearest one walking
/// up from `dir`, stopping at the git root — the same containment rule the
/// harnesses use for project scope, so launching from a subdirectory of a
/// repo still finds the repo's file.
pub fn find_config(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let candidate = d.join(".mcp.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        // The git root is the last directory searched: a `.mcp.json` above
        // the repo belongs to some other context.
        if d.join(".git").exists() {
            return None;
        }
        cur = d.parent();
    }
    None
}

/// Pack the MCP servers a launch from `dir` should carry, or `None` when
/// there is nothing to send (pref off, no `.mcp.json`, or every entry
/// filtered out). An unreadable or malformed file is an error — a repo that
/// commits one expects it to work, and silently launching without it would
/// change what the agent can do without telling anyone.
pub fn pack(prefs: &AgentPrefs, dir: &Path) -> Result<Option<PackedMcp>> {
    if !prefs.mcp.enabled {
        return Ok(None);
    }
    let Some(source_path) = find_config(dir) else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(&source_path)
        .with_context(|| format!("Failed to read {}", source_path.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", source_path.display()))?;
    let Some(servers) = parsed.get("mcpServers").and_then(|v| v.as_object()) else {
        return Ok(None);
    };

    // BTreeMap for a deterministic serialization, which is what makes the
    // hash stable across launches.
    let mut shipped: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for (name, config) in servers {
        if RESERVED_NAMES.contains(&name.as_str())
            || prefs.mcp.exclude.iter().any(|e| e == name)
            || config.get("disabled").and_then(|d| d.as_bool()) == Some(true)
        {
            continue;
        }
        let mut config = config.clone();
        // `disabled: false` is mono's own convention, not the harnesses' —
        // strip it rather than teach every reader about it.
        if let Some(obj) = config.as_object_mut() {
            obj.remove("disabled");
        }
        // A name that already leads with the prefix keeps it single —
        // `user-user-foo` helps nobody.
        let shipped_name = if name.starts_with(NAME_PREFIX) {
            name.clone()
        } else {
            format!("{NAME_PREFIX}{name}")
        };
        shipped.insert(shipped_name, config);
    }
    if shipped.is_empty() {
        return Ok(None);
    }

    let payload = serde_json::to_vec(&shipped)?;
    if payload.len() > MAX_BYTES {
        anyhow::bail!(
            "{} is too large to sync ({} bytes, limit {MAX_BYTES}).",
            source_path.display(),
            payload.len()
        );
    }
    let hash = format!("{:x}", Sha256::digest(&payload));
    Ok(Some(PackedMcp {
        payload,
        hash,
        names: shipped.into_keys().collect(),
        source_path,
    }))
}

/// Marker prefix the launcher greps out of the main provision script's stdout
/// to decide whether the agent already holds this exact server set.
pub const REMOTE_HASH_MARKER: &str = "MCP-HASH:";

/// Where the hash of the last synced set lives on the agent.
pub const REMOTE_HASH_FILE: &str = "$HOME/.railway-mcp-hash";

/// Reads the hash the agent recorded, out of the provision script's stdout.
pub fn parse_remote_hash(provision_output: &str) -> Option<String> {
    provision_output
        .lines()
        .find_map(|line| line.trim().strip_prefix(REMOTE_HASH_MARKER))
        .map(str::to_string)
        .filter(|h| !h.is_empty())
}

/// The VM-side sync. The filtered `mcpServers` object arrives on stdin; both
/// harness configs are merged add-only under jq, each through a temp file so
/// a failed merge cannot leave a half-written config. Every step degrades
/// instead of aborting the launch, and the hash file is written last so a
/// failure anywhere above means the next launch retries rather than believing
/// itself current.
pub fn provision_script(hash: &str) -> String {
    format!(
        r#"umask 077
# Read stdin FIRST, before anything that can bail: exiting with the pipe still
# full breaks the CLI's write instead of the sync.
payload="$HOME/.railway-mcp-payload.json"
cat > "$payload"
command -v jq >/dev/null 2>&1 || {{ rm -f "$payload"; echo MCP-NO-JQ; exit 0; }}
jq -e 'type == "object"' "$payload" >/dev/null 2>&1 || {{ rm -f "$payload"; echo MCP-BAD-JSON; exit 0; }}
# The canonical copy, for the platform: express-agent's boot reconcile reads
# this and renders the servers into every harness dialect it has verified —
# the TOML ones (codex, grok) included, which the shell merges below cannot
# reach. The direct merges stay as the compatibility path for images whose
# express-agent predates the file.
cp "$payload" "$HOME/.railway-mcp.json"
ok=1
# claude's user scope: mcpServers object. Add-only — existing names (the
# user's own, or express-agent's) always win; ours fill the gaps.
cfg="$HOME/.claude.json"
[ -s "$cfg" ] || echo '{{}}' > "$cfg"
if jq --slurpfile new "$payload" '.mcpServers = ($new[0] + (.mcpServers // {{}}))' "$cfg" > "$cfg.railway-mcp-tmp" 2>/dev/null; then
  mv "$cfg.railway-mcp-tmp" "$cfg"
else
  rm -f "$cfg.railway-mcp-tmp"; ok=0
fi
# The railway harness reads settings.json's mcp_servers array. Same rule:
# only names not already present are appended, translated into the array
# dialect ({{name, transport, url|command}}).
mkdir -p "$HOME/.claude"
set="$HOME/.claude/settings.json"
[ -s "$set" ] || echo '{{}}' > "$set"
if jq --slurpfile new "$payload" '
  (.mcp_servers // []) as $have
  | ($have | map(.name)) as $names
  | .mcp_servers = $have + ($new[0]
      | to_entries
      | map(select(.key as $k | $names | index($k) | not))
      | map({{name: .key}}
          + (if .value.url then {{transport: (.value.type // "http"), url: .value.url}}
             else {{transport: "stdio", command: (.value.command // ""), args: (.value.args // [])}} end)
          + (if .value.headers then {{headers: .value.headers}} else {{}} end)
          + (if .value.env then {{env: .value.env}} else {{}} end)))
' "$set" > "$set.railway-mcp-tmp" 2>/dev/null; then
  mv "$set.railway-mcp-tmp" "$set"
else
  rm -f "$set.railway-mcp-tmp"; ok=0
fi
rm -f "$payload"
[ "$ok" = 1 ] || {{ echo MCP-MERGE-FAILED; exit 0; }}
printf '%s\n' '{hash}' > "{REMOTE_HASH_FILE}"
echo MCP-OK"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs_on() -> AgentPrefs {
        AgentPrefs::default()
    }

    fn plant(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(".mcp.json"), body).unwrap();
    }

    const MONO_LIKE: &str = r#"{
        "mcpServers": {
            "railway-internal": { "type": "http", "url": "https://mcp.internal.example.com/" },
            "notion": { "type": "http", "url": "https://mcp.notion.com/mcp", "disabled": true },
            "buildkite": { "type": "http", "url": "https://mcp.buildkite.com/mcp", "disabled": false },
            "railway": { "type": "http", "url": "https://should-never-ship.example.com" },
            "local-tool": { "command": "my-mcp", "args": ["--serve"] }
        }
    }"#;

    #[test]
    fn packs_enabled_servers_and_filters_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        plant(dir.path(), MONO_LIKE);

        let packed = pack(&prefs_on(), dir.path()).unwrap().unwrap();
        assert_eq!(
            packed.names,
            vec![
                "user-buildkite".to_string(),
                "user-local-tool".to_string(),
                "user-railway-internal".to_string()
            ],
            "disabled entries and the platform's own names stay home; \
             what ships wears the import prefix"
        );
        // The mono-only `disabled` flag is stripped from what ships.
        let shipped: serde_json::Value = serde_json::from_slice(&packed.payload).unwrap();
        assert!(shipped["user-buildkite"].get("disabled").is_none());
        assert_eq!(
            shipped["user-buildkite"]["url"],
            "https://mcp.buildkite.com/mcp"
        );
    }

    /// A source name that already leads with the prefix keeps it single.
    #[test]
    fn the_prefix_never_doubles() {
        let dir = tempfile::tempdir().unwrap();
        plant(
            dir.path(),
            r#"{"mcpServers": {"user-foo": {"type": "http", "url": "https://x.example"}}}"#,
        );
        let packed = pack(&prefs_on(), dir.path()).unwrap().unwrap();
        assert_eq!(packed.names, vec!["user-foo".to_string()]);
    }

    #[test]
    fn walks_up_to_the_git_root_and_not_past_it() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let deep = repo.join("packages").join("thing");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        plant(&repo, MONO_LIKE);

        assert_eq!(
            find_config(&deep).unwrap(),
            repo.join(".mcp.json"),
            "a launch from a subdirectory finds the repo's file"
        );

        // A file ABOVE the repo belongs to some other context.
        let bare = root.path().join("bare");
        std::fs::create_dir_all(bare.join(".git")).unwrap();
        plant(root.path(), MONO_LIKE);
        assert!(find_config(&bare).is_none());
    }

    #[test]
    fn disabled_pref_missing_file_or_empty_set_pack_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(pack(&prefs_on(), dir.path()).unwrap().is_none(), "no file");

        plant(
            dir.path(),
            r#"{"mcpServers": {"railway": {"type": "http", "url": "x"}}}"#,
        );
        assert!(
            pack(&prefs_on(), dir.path()).unwrap().is_none(),
            "everything filtered"
        );

        plant(dir.path(), MONO_LIKE);
        let mut off = prefs_on();
        off.mcp.enabled = false;
        assert!(pack(&off, dir.path()).unwrap().is_none(), "pref off");
    }

    #[test]
    fn excluded_names_stay_home() {
        let dir = tempfile::tempdir().unwrap();
        plant(dir.path(), MONO_LIKE);
        let mut prefs = prefs_on();
        // Excludes match the `.mcp.json` names, not the prefixed form.
        prefs.mcp.exclude = vec!["railway-internal".into()];
        let packed = pack(&prefs, dir.path()).unwrap().unwrap();
        assert!(!packed.names.iter().any(|n| n.contains("railway-internal")));
    }

    #[test]
    fn malformed_json_is_an_error_not_a_silent_skip() {
        let dir = tempfile::tempdir().unwrap();
        plant(dir.path(), "{ not json");
        assert!(pack(&prefs_on(), dir.path()).is_err());
    }

    /// Content-addressed: an unchanged file hashes the same across launches,
    /// and any change moves it.
    #[test]
    fn hash_tracks_content() {
        let dir = tempfile::tempdir().unwrap();
        plant(dir.path(), MONO_LIKE);
        let first = pack(&prefs_on(), dir.path()).unwrap().unwrap();
        let again = pack(&prefs_on(), dir.path()).unwrap().unwrap();
        assert_eq!(first.hash, again.hash);

        plant(
            dir.path(),
            r#"{"mcpServers": {"other": {"type": "http", "url": "https://x.example"}}}"#,
        );
        let changed = pack(&prefs_on(), dir.path()).unwrap().unwrap();
        assert_ne!(first.hash, changed.hash);
    }

    #[test]
    fn parses_the_remote_hash_marker() {
        assert_eq!(
            parse_remote_hash("AGENT-READY\nMCP-HASH:abc\n").as_deref(),
            Some("abc")
        );
        assert!(parse_remote_hash("AGENT-READY\nMCP-HASH:\n").is_none());
        assert!(parse_remote_hash("AGENT-READY\n").is_none());
    }

    /// Every early exit must come after the `cat`, and the hash is recorded
    /// only after both merges succeeded.
    #[test]
    fn provision_script_drains_stdin_first_and_records_the_hash_last() {
        let script = provision_script("deadbeef");
        let cat_at = script.find("cat > \"$payload\"").unwrap();
        for marker in ["MCP-NO-JQ", "MCP-BAD-JSON", "MCP-MERGE-FAILED"] {
            assert!(
                script.find(marker).unwrap() > cat_at,
                "`{marker}` can be reached before stdin is drained"
            );
        }
        let hash_at = script.find("deadbeef").unwrap();
        let ok_at = script.find("echo MCP-OK").unwrap();
        let merge_at = script.find("mcpServers =").unwrap();
        assert!(merge_at < hash_at && hash_at < ok_at);
    }

    /// The script actually runs: executed against a stand-in `$HOME` with the
    /// real payload on stdin. Asserts the add-only rule — a name express-agent
    /// (or the user) already put on the agent is never replaced — and both
    /// dialects. Skipped where jq isn't installed locally; the agent image
    /// bakes it.
    #[cfg(unix)]
    #[test]
    fn provision_script_merges_add_only_into_both_dialects() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        if Command::new("jq").arg("--version").output().is_err() {
            eprintln!("skipping: jq not installed");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        plant(dir.path(), MONO_LIKE);
        let packed = pack(&prefs_on(), dir.path()).unwrap().unwrap();

        // Stand-in agent: express-agent has already written its own entries,
        // and one prefixed name is already taken (an earlier import, or a
        // user's own) — the add-only rule must leave it alone.
        let vm = tempfile::tempdir().unwrap();
        std::fs::write(
            vm.path().join(".claude.json"),
            r#"{"hasCompletedOnboarding": true, "mcpServers": {"user-buildkite": {"type": "http", "url": "https://theirs.example"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(vm.path().join(".claude")).unwrap();
        std::fs::write(
            vm.path().join(".claude").join("settings.json"),
            r#"{"mcp_servers": [{"name": "railway", "transport": "stdio", "command": "express-agent"}, {"name": "user-buildkite", "transport": "http", "url": "https://theirs.example"}]}"#,
        )
        .unwrap();

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(provision_script(&packed.hash))
            .env("HOME", vm.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&packed.payload)
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("MCP-OK"),
            "stdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // ~/.claude.json: ours added under their prefixed names, the taken
        // name untouched, the unrelated fields intact.
        let cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(vm.path().join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(cfg["hasCompletedOnboarding"], true);
        assert_eq!(
            cfg["mcpServers"]["user-buildkite"]["url"], "https://theirs.example",
            "an existing name is never replaced"
        );
        assert_eq!(
            cfg["mcpServers"]["user-railway-internal"]["url"],
            "https://mcp.internal.example.com/"
        );

        // ~/.claude/settings.json: array dialect, existing entries first,
        // the taken name not appended a second time.
        let set: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(vm.path().join(".claude").join("settings.json")).unwrap(),
        )
        .unwrap();
        let servers = set["mcp_servers"].as_array().unwrap();
        assert_eq!(servers[0]["name"], "railway", "existing entries lead");
        let buildkites: Vec<_> = servers
            .iter()
            .filter(|s| s["name"] == "user-buildkite")
            .collect();
        assert_eq!(buildkites.len(), 1, "a taken name is not duplicated");
        assert_eq!(buildkites[0]["url"], "https://theirs.example");
        let internal = servers
            .iter()
            .find(|s| s["name"] == "user-railway-internal")
            .expect("http server appended");
        assert_eq!(internal["transport"], "http");
        let local = servers
            .iter()
            .find(|s| s["name"] == "user-local-tool")
            .expect("stdio server appended");
        assert_eq!(local["transport"], "stdio");
        assert_eq!(local["command"], "my-mcp");

        // The canonical copy for the platform's own reconcile landed, and it
        // is exactly the payload.
        let canonical = std::fs::read(vm.path().join(".railway-mcp.json")).unwrap();
        assert_eq!(canonical, packed.payload);

        // The hash was recorded, so the next launch skips the upload; no
        // scratch files left behind.
        let recorded = std::fs::read_to_string(vm.path().join(".railway-mcp-hash")).unwrap();
        assert_eq!(recorded.trim(), packed.hash);
        assert!(!vm.path().join(".railway-mcp-payload.json").exists());
    }
}
