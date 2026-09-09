//! Run explicit upgrades against a throwaway installation and package manager.
//! No published releases, user skills, or real package managers are modified.
#![cfg(unix)]

use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::Path, process::Command};

fn install_fixture(home: &Path, script: &str) -> Command {
    install_fixture_at(home, "node_modules/railway/railway", "npm", script)
}

fn install_fixture_at(home: &Path, path: &str, manager: &str, script: &str) -> Command {
    let executable = home.join(path);
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_railway"), &executable).unwrap();
    let bin = home.join("mock-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let npm = bin.join(manager);
    std::fs::write(&npm, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new(&executable);
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", bin)
        .env("TEST_RAILWAY_EXE", executable)
        .env("DO_NOT_TRACK", "1")
        .env("NO_COLOR", "1")
        .env("RAILWAY_NO_AUTO_UPDATE", "1")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .args(["upgrade", "--yes"]);
    command
}

#[test]
fn homebrew_upgrade_verifies_the_stable_entry_point_after_the_cellar_path_moves() {
    let home = tempfile::tempdir().unwrap();
    let script = r#"
/bin/mkdir -p "$HOME/homebrew/opt/railway/bin"
printf '#!/bin/sh\necho railway 255.255.254\n' > "$HOME/homebrew/opt/railway/bin/railway"
/bin/chmod +x "$HOME/homebrew/opt/railway/bin/railway"
/bin/rm "$TEST_RAILWAY_EXE"
"#;
    let output = install_fixture_at(
        home.path(),
        "homebrew/Cellar/railway/old/bin/railway",
        "brew",
        script,
    )
    .output()
    .unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("v255.255.254 will be used on your next command"),
        "{text}"
    );
}

const INSTALL: &str = r#"
printf '#!/bin/sh\necho railway 255.255.254\n' > "$TEST_RAILWAY_EXE.next"
/bin/chmod +x "$TEST_RAILWAY_EXE.next"
/bin/mv "$TEST_RAILWAY_EXE.next" "$TEST_RAILWAY_EXE"
echo 'package manager chatter'
"#;

#[test]
fn explicit_upgrade_shows_verified_cli_and_skill_outcomes_even_with_auto_updates_off() {
    let home = tempfile::tempdir().unwrap();
    let output = install_fixture(home.path(), INSTALL).output().unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}");
    assert!(output.stdout.is_empty());
    assert!(text.contains("Updating Railway"), "{text}");
    assert!(text.contains("✓ CLI installed"), "{text}");
    assert!(
        text.contains("No CLI-managed agent skills installed"),
        "{text}"
    );
    assert!(
        text.contains("v255.255.254 will be used on your next command"),
        "{text}"
    );
    assert!(!text.contains("package manager chatter"));
    let status: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.path().join(".railway/update-status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["installed_version"], "255.255.254");
    assert_eq!(status["skills"]["cli_version"], "255.255.254");
    assert_eq!(status["skills"]["outcome"]["status"], "not_installed");
    assert_eq!(status["last_notified_version"], "255.255.254");
}

#[test]
fn package_manager_failure_keeps_diagnostics_and_never_claims_completion() {
    let home = tempfile::tempdir().unwrap();
    let output = install_fixture(home.path(), "echo 'registry unavailable' >&2\nexit 7")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(text.contains("registry unavailable"), "{text}");
    assert!(text.contains("CLI installation failed"), "{text}");
    assert!(!text.contains("✓ CLI installed"));
    assert!(!text.contains("Ready."));
    assert!(!home.path().join(".railway/update-status.json").exists());
}

#[test]
fn cli_success_with_skill_failure_is_recorded_as_partial_success() {
    let home = tempfile::tempdir().unwrap();
    let target = home.path().join(".agents/skills");
    std::fs::create_dir_all(home.path().join(".railway")).unwrap();
    std::fs::write(home.path().join(".railway/skills.json"), json!({
        "targets": {target.to_str().unwrap(): {"use-railway": {"installed_at": "t", "files": {}}}}
    }).to_string()).unwrap();
    let output = install_fixture(home.path(), INSTALL).output().unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("✓ CLI installed"), "{text}");
    assert!(
        text.contains("Agent skills could not be synchronized"),
        "{text}"
    );
    assert!(text.contains("railway autoupdate status"), "{text}");
    assert!(!text.contains("✓ Agent skills synchronized"));
    let status: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.path().join(".railway/update-status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["installed_version"], "255.255.254");
    assert_eq!(status["skills"]["outcome"]["status"], "failed");
}
