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

    let info = code::connect_info(&agent.environment_id, &agent.id).await?;
    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || {
        run_native_ssh_captured(
            &info.ssh_target,
            &script(),
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
  elif herdr workspace create --label app --cwd /app >/dev/null 2>&1; then
    echo "workspace app: created (/app)"
    if command -v python3 >/dev/null 2>&1; then
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
echo BOOTSTRAP-OK
"##;

fn script() -> String {
    format!("{HARNESS_PATH}\n{BODY}")
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

        use super::super::script;

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
                    "#!/bin/bash\necho \"$*\" >> \"$HOME/herdr.log\"\nif [ \"$1 $2\" = \"workspace list\" ]; then cat <<'EOF'\n{workspaces}\nEOF\nfi\n"
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
                    .arg(script())
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
                "[terminal]\nshell_mode = \"login\"\ndefault_shell = \"/bin/bash\"\n\n[experimental]\npane_history = true\n"
            );
            let profile = std::fs::read_to_string(vm.home.path().join(".profile")).unwrap();
            assert!(profile.contains(".claude-code-env"), "{profile}");
            assert_eq!(
                vm.herdr_calls(),
                vec![
                    "integration install claude",
                    "integration install codex",
                    "server reload-config",
                    "workspace list",
                    "workspace create --label app --cwd /app",
                    "workspace list",
                ]
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
                "[theme]\nname = \"dark\"\n\n[terminal]\nshell_mode = \"login\"\nscrollback = 1\n\ndefault_shell = \"/bin/bash\"\n[keys]\nshell_mode = \"x\"\n\n[experimental]\npane_history = true\n"
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
                "[terminal]\nshell_mode = \"login\"\ndefault_shell = \"/bin/bash\"\n[experimental]\npane_history = true\n"
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
