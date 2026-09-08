//! One remote script over the relay, run once per agent: herdr integrations,
//! the two config.toml keys herdr's machine mode needs, and an `app` workspace.

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;

use crate::client::GQLClient;
use crate::commands::code;
use crate::commands::code::{HARNESS_PATH, LaunchArgs, Progress};
use crate::commands::ssh::native::run_native_ssh_captured;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;

#[derive(Parser)]
pub struct Args {
    /// Agent name or ID
    pub agent: Option<String>,

    #[clap(flatten)]
    harness: super::harness::HarnessFlags,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (agent, _) = ca::resolve(&configs, &client, args.agent.as_deref(), None).await?;
    let harness = super::harness::choose(&args.harness, false)?;
    run(&agent, harness).await
}

/// Prepare a running agent's VM for herdr. Idempotent; `new` calls this right
/// after `herdr machine add`.
pub async fn run(agent: &ca::Agent, harness: &str) -> Result<()> {
    if !matches!(agent.status, ca::Status::Running) {
        bail!(
            "Agent {} is {}. Wake it first: {}",
            agent.name,
            agent.status.label(),
            format!("railway ca wake {}", agent.name).cyan()
        );
    }
    // App mode: the same provisioning `railway ca desktop` does (credential,
    // skills, MCP) without the ~/.profile autostart, which would launch the
    // harness on top of every herdr login shell.
    let mut launch = LaunchArgs::for_app_mode(
        harness,
        Some(agent.project_id.clone()),
        Some(agent.environment_id.clone()),
    );
    launch.agent_id = Some(agent.id.clone());
    let progress = code::CliProgress::default();
    let prepared = code::prepare(&launch, &progress, code::SessionStyle::FullTerminal).await;
    progress.finish();
    prepared.with_context(|| format!("Provisioning {} for {harness}", agent.name))?;
    println!("  provisioned {harness}: skills, MCP, credential when one was carried");

    let remote = Remote::new(&Configs::new()?.get_backboard(), harness)?;
    let info = code::connect_info(&agent.environment_id, &agent.id).await?;
    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || {
        run_native_ssh_captured(
            &info.ssh_target,
            &script(&remote),
            info.identity.as_deref(),
            None,
            &info.relay_opts,
        )
    })
    .await??;
    let stdout = String::from_utf8_lossy(&stdout);
    match outcome(&stdout) {
        Outcome::Ok(lines) => {
            for line in lines {
                println!("  {line}");
            }
            println!("✓ Bootstrapped agent {} for herdr", agent.name.cyan());
            Ok(())
        }
        Outcome::HerdrMissing => {
            println!(
                "{} herdr is not installed on agent {}; `herdr machine add` installs it. Re-run {} afterwards.",
                "!".yellow(),
                agent.name.cyan(),
                format!("railway ca herdr bootstrap {}", agent.name).cyan()
            );
            Ok(())
        }
        Outcome::NoMarker => bail!(
            "Bootstrap of agent {} produced no status marker (ssh exit {code}).\n{}\n{}",
            agent.name,
            stdout.trim(),
            String::from_utf8_lossy(&stderr).trim()
        ),
    }
}

enum Outcome<'a> {
    Ok(Vec<&'a str>),
    HerdrMissing,
    NoMarker,
}

fn outcome(stdout: &str) -> Outcome<'_> {
    let lines: Vec<&str> = stdout.lines().map(str::trim).collect();
    if lines.contains(&"HERDR-MISSING") {
        return Outcome::HerdrMissing;
    }
    if !lines.contains(&"BOOTSTRAP-OK") {
        return Outcome::NoMarker;
    }
    Outcome::Ok(
        lines
            .into_iter()
            .filter(|l| !l.is_empty() && *l != "BOOTSTRAP-OK")
            .collect(),
    )
}

/// `set_table_key` edits one key inside one `[table]`: replaces the line
/// (commented or not) when present, appends to the table otherwise, and creates
/// the table at the end when it is missing. Other tables are never touched.
const BODY: &str = r##"if ! command -v herdr >/dev/null 2>&1; then echo HERDR-MISSING; exit 0; fi
for tool in claude codex; do
  if herdr integration install "$tool" >/dev/null 2>&1; then
    echo "integration $tool: ok"
  else
    echo "integration $tool: skipped (herdr integration install $tool failed)"
  fi
done
cfg="$HOME/.config/herdr/config.toml"
mkdir -p "$(dirname "$cfg")"
[ -f "$cfg" ] || : > "$cfg"
before="$(cat "$cfg")"
set_table_key() {
  awk -v table="$1" -v key="$2" -v line="$3" '
    BEGIN { in_t = 0; seen = 0; done = 0 }
    /^[[:space:]]*\[/ {
      if (in_t && !done) { print line; done = 1 }
      in_t = ($0 ~ "^[[:space:]]*\\[" table "\\][[:space:]]*$")
      if (in_t) seen = 1
    }
    in_t && !done && $0 ~ "^[[:space:]]*#?[[:space:]]*" key "[[:space:]]*=" { print line; done = 1; next }
    { print }
    END {
      if (!done) {
        if (!seen) { if (NR > 0) print ""; print "[" table "]" }
        print line
      }
    }
  ' "$cfg" > "$cfg.tmp" && mv "$cfg.tmp" "$cfg"
}
set_table_key terminal shell_mode 'shell_mode = "login"'
set_table_key terminal default_shell 'default_shell = "/bin/bash"'
set_table_key experimental pane_history 'pane_history = true'
if [ "$before" = "$(cat "$cfg")" ]; then
  echo "config: pane_history, shell_mode = login, default_shell = /bin/bash (unchanged)"
else
  echo "config: pane_history, shell_mode = login, default_shell = /bin/bash (updated)"
  herdr server reload-config >/dev/null 2>&1 && echo "config: reloaded"
fi
prof="$HOME/.profile"
if grep -q "railway ca herdr env" "$prof" 2>/dev/null; then
  echo "profile: env block present"
else
  cat >> "$prof" <<'PROFEOF'

# railway ca herdr env
[ -f "$HOME/.claude-code-env" ] && set -a && . "$HOME/.claude-code-env" && set +a
[ -f "$HOME/.gh-token" ] && export GH_TOKEN="$(cat "$HOME/.gh-token")"
PROFEOF
  echo "profile: env block added"
fi
if ws="$(herdr workspace list 2>/dev/null)"; then
  if command -v python3 >/dev/null 2>&1; then
    has="$(printf '%s' "$ws" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(int(any(w.get("label")=="app" for w in d.get("result",{}).get("workspaces",[]))))' 2>/dev/null || echo 0)"
  elif printf '%s' "$ws" | grep -Eq '"label"[[:space:]]*:[[:space:]]*"app"'; then
    has=1
  else
    has=0
  fi
  if [ "$has" = 1 ]; then
    echo "workspace app: exists"
  elif created="$(herdr workspace create --label app --cwd /app 2>/dev/null)"; then
    echo "workspace app: created (/app)"
    if command -v python3 >/dev/null 2>&1; then
      root="$(printf '%s' "$created" | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["root_pane"]["pane_id"])' 2>/dev/null)"
      if [ -n "$root" ] && [ -n "@HARNESS_CMD@" ]; then
        sleep 2
        herdr pane run "$root" "@HARNESS_CMD@" >/dev/null 2>&1 && echo "started @HARNESS_CMD@ in the app workspace"
      fi
      bare="$(herdr workspace list 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(" ".join(w["workspace_id"] for w in d.get("result",{}).get("workspaces",[]) if w.get("label")=="/" and w.get("pane_count")==1 and w.get("agent_status") in (None,"unknown")))' 2>/dev/null)"
      for id in $bare; do
        herdr workspace close "$id" >/dev/null 2>&1 && echo "workspace /: closed (bare startup shell)"
      done
    fi
  else
    echo "workspace app: create failed"
  fi
else
  echo "workspace app: skipped (no herdr server running; connecting starts one)"
fi
"##;

/// The plugin the VM's herdr server publishes, so the same keys work while
/// this machine is selected. `cfg` is the config path BODY established.
const REMOTE_SEGMENT: &str = r##"pdir="$HOME/.config/railway-ca-herdr-plugin"
mkdir -p "$pdir"
cat > "$pdir/herdr-plugin.toml" <<'RAILWAY_CA_MANIFEST'
@MANIFEST@
RAILWAY_CA_MANIFEST
cat > "$pdir/agents.sh" <<'RAILWAY_CA_AGENTS'
@AGENTS_SH@
RAILWAY_CA_AGENTS
cat > "$pdir/sleep-self.sh" <<'RAILWAY_CA_SLEEP'
@SLEEP_SH@
RAILWAY_CA_SLEEP
chmod 755 "$pdir"/*.sh
if herdr plugin list 2>/dev/null | grep -q "railway\.ca "; then
  echo "remote plugin: linked"
elif herdr plugin link "$pdir" >/dev/null 2>&1; then
  echo "remote plugin: linked (new)"
else
  echo "remote plugin: link failed"
fi
keys_block="$(printf '%s\n%s\n' "@KEYS_MARKER@" '@KEYS@')"
if grep -q "@KEYS_MARKER@" "$cfg" 2>/dev/null && command -v python3 >/dev/null 2>&1; then
  KEYS_BLOCK="$keys_block" python3 - "$cfg" <<'RAILWAY_CA_PY'
import os, re, sys
path = sys.argv[1]; text = open(path).read(); block = os.environ["KEYS_BLOCK"]
marker = block.splitlines()[0]
kept = []; lines = text.split("\n"); i = 0
while i < len(lines):
    if lines[i].strip() == marker:
        i += 1; continue
    if lines[i].strip() == "[[keys.command]]":
        j = i + 1
        while j < len(lines) and not lines[j].lstrip().startswith("["): j += 1
        if any('"railway.ca.' in l for l in lines[i:j]):
            while j > i + 1 and not lines[j - 1].strip(): j -= 1
            i = j; continue
    kept.append(lines[i]); i += 1
base = "\n".join(kept).rstrip("\n")
new = (base + "\n\n" if base else "") + block + "\n"
if new != text:
    open(path, "w").write(new); print("keys: updated")
else:
    print("keys: present")
RAILWAY_CA_PY
elif grep -q "@KEYS_MARKER@" "$cfg" 2>/dev/null; then
  echo "keys: present"
else
  printf '\n%s\n' "$keys_block" >> "$cfg"
  echo "keys: added"
fi
herdr server reload-config >/dev/null 2>&1 && echo "config: reloaded"
"##;

const AGENTS_SH: &str = r##"#!/bin/sh
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"
if [ "$1" = "--open" ]; then
  exec "${HERDR_BIN_PATH:-herdr}" plugin pane open --plugin railway.ca --entrypoint agents
fi
if railway ca herdr --help >/dev/null 2>&1; then
  exec railway ca herdr agents --remote
fi
echo "railway on this VM ($(railway --version 2>/dev/null)) predates 'railway ca herdr'."
echo "Select Local and press the same key for the full picker, or upgrade railway here."
sleep 6"##;

const SLEEP_SH: &str = r##"#!/bin/sh
set -eu
: "${RAILWAY_API_TOKEN:?RAILWAY_API_TOKEN is not in the herdr server env}"
: "${RAILWAY_CLOUD_AGENT_ID:?RAILWAY_CLOUD_AGENT_ID is not in the herdr server env}"
"${HERDR_BIN_PATH:-herdr}" notification show "Sleeping this agent" --body "railway: cloudAgentSleep issued from the VM; disable or re-sync its machine from Local" --sound none >/dev/null 2>&1 || true
sync
curl -fsS -m 20 "@BACKBOARD@" \
  -H "Authorization: Bearer $RAILWAY_API_TOKEN" -H "Content-Type: application/json" \
  -d "{\"query\":\"mutation(\$id: ID!) { cloudAgentSleep(id: \$id) { id status } }\",\"variables\":{\"id\":\"$RAILWAY_CLOUD_AGENT_ID\"}}"
echo"##;

struct Remote {
    manifest: String,
    backboard: String,
    harness_cmd: String,
}

impl Remote {
    fn new(backboard: &str, harness: &str) -> Result<Self> {
        Ok(Self {
            manifest: super::install::Manifest::remote().render()?,
            backboard: backboard.to_string(),
            harness_cmd: harness_command(harness).to_string(),
        })
    }

    fn segment(&self) -> String {
        REMOTE_SEGMENT
            .replace("@MANIFEST@", self.manifest.trim_end())
            .replace("@AGENTS_SH@", AGENTS_SH)
            .replace(
                "@SLEEP_SH@",
                &SLEEP_SH.replace("@BACKBOARD@", &self.backboard),
            )
            .replace("@KEYS_MARKER@", super::install::KEYS_MARKER)
            .replace("@KEYS@", super::install::REMOTE_KEYBINDING.trim_end())
    }
}

/// What to type into the fresh /app pane. Railway's harness has its own binary
/// name; the others match their slug. Unknown slugs start nothing.
fn harness_command(harness: &str) -> &'static str {
    match harness {
        "claude" => "claude",
        "codex" => "codex",
        "grok" => "grok",
        "railway" => "railway-agent-tui",
        _ => "",
    }
}

fn script(remote: &Remote) -> String {
    format!(
        "{HARNESS_PATH}\n{}\n{}\necho BOOTSTRAP-OK\n",
        BODY.replace("@HARNESS_CMD@", &remote.harness_cmd),
        remote.segment()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_reads_markers() {
        assert!(matches!(outcome("HERDR-MISSING\n"), Outcome::HerdrMissing));
        assert!(matches!(
            outcome("integration claude: ok\n"),
            Outcome::NoMarker
        ));
        match outcome("integration claude: ok\n\nworkspace app: exists\nBOOTSTRAP-OK\n") {
            Outcome::Ok(lines) => assert_eq!(
                lines,
                vec!["integration claude: ok", "workspace app: exists"]
            ),
            _ => panic!("expected Ok"),
        }
    }

    #[cfg(unix)]
    mod script {
        use std::process::Command;

        use super::super::{Remote, script};

        fn remote() -> Remote {
            Remote::new("https://backboard.example/graphql/v2", "claude").unwrap()
        }

        fn keys_tail() -> String {
            format!(
                "\n{}\n{}",
                crate::commands::cloud_agent::herdr::install::KEYS_MARKER,
                crate::commands::cloud_agent::herdr::install::REMOTE_KEYBINDING
            )
        }

        const WORKSPACES_WITHOUT_APP: &str =
            r#"{"result":{"workspaces":[{"label":"default","cwd":"/root"}]}}"#;
        const WORKSPACES_WITH_APP: &str =
            r#"{"result":{"workspaces":[{"label":"default"},{"label":"app","cwd":"/app"}]}}"#;

        struct Vm {
            home: tempfile::TempDir,
        }

        impl Vm {
            fn new(workspaces: &str) -> Self {
                let vm = Self {
                    home: tempfile::tempdir().unwrap(),
                };
                vm.install_herdr(&format!(
                    "#!/bin/bash\necho \"$*\" >> \"$HOME/herdr.log\"\nif [ \"$1 $2\" = \"workspace list\" ]; then cat <<'EOF'\n{workspaces}\nEOF\nfi\nif [ \"$1 $2\" = \"workspace create\" ]; then echo '{{\"result\":{{\"root_pane\":{{\"pane_id\":\"w9:p1\"}}}}}}'; fi\n"
                ));
                vm
            }

            fn install_herdr(&self, shim: &str) {
                use std::os::unix::fs::PermissionsExt;
                let path = self.herdr_path();
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, shim).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }

            fn herdr_path(&self) -> std::path::PathBuf {
                self.home.path().join(".local/bin/herdr")
            }

            fn run(&self) -> String {
                let out = Command::new("bash")
                    .arg("-c")
                    .arg(script(&remote()))
                    .env_clear()
                    .env("HOME", self.home.path())
                    .env("PATH", "/usr/bin:/bin")
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "stderr: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                String::from_utf8(out.stdout).unwrap()
            }

            fn config_path(&self) -> std::path::PathBuf {
                self.home.path().join(".config/herdr/config.toml")
            }

            fn config(&self) -> String {
                std::fs::read_to_string(self.config_path()).unwrap_or_default()
            }

            fn write_config(&self, text: &str) {
                let path = self.config_path();
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }

            fn herdr_calls(&self) -> Vec<String> {
                std::fs::read_to_string(self.home.path().join("herdr.log"))
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            }
        }

        #[test]
        fn fresh_vm_gets_config_integrations_and_workspace() {
            let vm = Vm::new(WORKSPACES_WITHOUT_APP);
            let out = vm.run();
            assert!(out.trim_end().ends_with("BOOTSTRAP-OK"), "{out}");
            assert!(out.contains("integration claude: ok"), "{out}");
            assert!(out.contains("integration codex: ok"), "{out}");
            assert!(out.contains("default_shell = /bin/bash (updated)"), "{out}");
            assert!(out.contains("profile: env block added"), "{out}");
            assert!(out.contains("workspace app: created"), "{out}");
            assert_eq!(
                vm.config(),
                format!(
                    "{}{}",
                    "[terminal]\nshell_mode = \"login\"\ndefault_shell = \"/bin/bash\"\n\n[experimental]\npane_history = true\n",
                    keys_tail()
                )
            );
            let profile = std::fs::read_to_string(vm.home.path().join(".profile")).unwrap();
            assert!(profile.contains(".claude-code-env"), "{profile}");
            let calls = vm.herdr_calls();
            assert_eq!(
                &calls[..5],
                [
                    "integration install claude",
                    "integration install codex",
                    "server reload-config",
                    "workspace list",
                    "workspace create --label app --cwd /app",
                ]
            );
            assert!(
                calls.iter().any(|c| c.starts_with("plugin link ")),
                "{calls:?}"
            );
            assert!(out.contains("remote plugin: linked (new)"), "{out}");
            assert!(out.contains("keys: added"), "{out}");
            let pdir = vm.home.path().join(".config/railway-ca-herdr-plugin");
            let manifest = std::fs::read_to_string(pdir.join("herdr-plugin.toml")).unwrap();
            assert!(manifest.contains("id = \"railway.ca\""), "{manifest}");
            let sleep = std::fs::read_to_string(pdir.join("sleep-self.sh")).unwrap();
            assert!(sleep.contains("cloudAgentSleep"), "{sleep}");
            assert!(
                sleep.contains("https://backboard.example/graphql/v2"),
                "{sleep}"
            );
            assert!(
                std::fs::read_to_string(pdir.join("agents.sh"))
                    .unwrap()
                    .contains("--remote")
            );
        }

        #[test]
        fn second_run_changes_nothing() {
            let vm = Vm::new(WORKSPACES_WITH_APP);
            vm.run();
            let first = vm.config();
            let out = vm.run();
            assert_eq!(vm.config(), first);
            assert!(out.contains("(unchanged)"), "{out}");
            assert!(out.contains("profile: env block present"), "{out}");
            assert!(out.contains("keys: present"), "{out}");
            assert!(out.contains("workspace app: exists"), "{out}");
            let profile = std::fs::read_to_string(vm.home.path().join(".profile")).unwrap();
            assert_eq!(
                profile.matches("railway ca herdr env").count(),
                1,
                "{profile}"
            );
            assert!(
                !vm.herdr_calls()
                    .iter()
                    .any(|c| c.starts_with("workspace create")),
                "{:?}",
                vm.herdr_calls()
            );
        }

        #[test]
        fn existing_config_keeps_other_keys_and_tables() {
            let vm = Vm::new(WORKSPACES_WITH_APP);
            vm.write_config(
                "[theme]\nname = \"dark\"\n\n[terminal]\n# shell_mode = \"auto\"\nscrollback = 1\n\n[keys]\nshell_mode = \"x\"\n",
            );
            vm.run();
            assert_eq!(
                vm.config(),
                format!(
                    "{}{}",
                    "[theme]\nname = \"dark\"\n\n[terminal]\nshell_mode = \"login\"\nscrollback = 1\n\ndefault_shell = \"/bin/bash\"\n[keys]\nshell_mode = \"x\"\n\n[experimental]\npane_history = true\n",
                    keys_tail()
                )
            );
        }

        #[test]
        fn existing_values_are_replaced_in_place() {
            let vm = Vm::new(WORKSPACES_WITH_APP);
            vm.write_config(
                "[terminal]\nshell_mode = \"non_login\"\ndefault_shell = \"/bin/zsh\"\n[experimental]\npane_history = false\n",
            );
            vm.run();
            assert_eq!(
                vm.config(),
                format!(
                    "{}{}",
                    "[terminal]\nshell_mode = \"login\"\ndefault_shell = \"/bin/bash\"\n[experimental]\npane_history = true\n",
                    keys_tail()
                )
            );
        }

        #[test]
        fn bare_startup_workspace_is_closed_after_app_is_created() {
            let vm = Vm::new(
                r#"{"result":{"workspaces":[{"workspace_id":"w1","label":"/","pane_count":1,"agent_status":"unknown"}]}}"#,
            );
            let out = vm.run();
            assert!(out.contains("workspace /: closed"), "{out}");
            assert!(
                vm.herdr_calls().contains(&"workspace close w1".to_string()),
                "{:?}",
                vm.herdr_calls()
            );
        }

        #[test]
        fn no_server_skips_the_workspace() {
            let vm = Vm::new("");
            vm.install_herdr(
                "#!/bin/bash\necho \"$*\" >> \"$HOME/herdr.log\"\n[ \"$1\" = workspace ] && exit 1\nexit 0\n",
            );
            let out = vm.run();
            assert!(out.contains("workspace app: skipped"), "{out}");
            assert!(out.trim_end().ends_with("BOOTSTRAP-OK"), "{out}");
        }

        #[test]
        fn missing_herdr_reports_and_exits_zero() {
            let vm = Vm::new("");
            std::fs::remove_file(vm.herdr_path()).unwrap();
            let out = vm.run();
            assert_eq!(out.trim(), "HERDR-MISSING");
            assert!(!vm.home.path().join(".config").exists());
        }
    }
}
