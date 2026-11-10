//! Exercise startup with a real TTY: piped output would hide the banner even
//! without honoring the persisted auto-update preference.
#![cfg(unix)]

use std::io::Read;
use std::path::Path;

use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde_json::json;

fn run_cli(home: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let (output, success) = run_cli_result(home, args, env);
    assert!(success, "{output}");
    output
}

fn run_cli_result(home: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, bool) {
    let pty = NativePtySystem::default()
        .openpty(PtySize::default())
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_railway"));
    cmd.env_clear();
    cmd.env("HOME", home);
    cmd.env("DO_NOT_TRACK", "1");
    cmd.env("NO_COLOR", "1");
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.cwd(home);
    cmd.args(args);
    let mut reader = pty.master.try_clone_reader().unwrap();
    let mut child = pty.slave.spawn_command(cmd).unwrap();
    drop(pty.slave);
    let output = std::thread::spawn(move || {
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        output
    });
    let success = child.wait().unwrap().success();
    (output.join().unwrap(), success)
}

fn seed_pending_updates(home: &Path) {
    std::fs::create_dir_all(home.join(".railway")).unwrap();
    std::fs::write(
        home.join(".railway/version.json"),
        json!({"latest_version": "255.255.255", "last_update_check": chrono::Utc::now()})
            .to_string(),
    )
    .unwrap();
    // Both a managed, locally blocked update and an unmanaged installation.
    let managed = home.join(".agents/skills");
    let orphan = home.join(".cursor/skills/use-railway");
    std::fs::create_dir_all(managed.join("use-railway")).unwrap();
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("SKILL.md"), "unmanaged").unwrap();
    std::fs::write(
        home.join(".railway/skills.json"),
        json!({
            "source_sha": "old",
            "latest_sha": "new",
            "auto_applied_sha": "new",
            "cli_version": env!("CARGO_PKG_VERSION"),
            "last_checked": chrono::Utc::now(),
            "targets": {managed.to_str().unwrap(): {
                "use-railway": {"installed_at": "t", "files": {}}
            }}
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn disabled_updates_suppress_tty_notices_and_background_checks() {
    for (preference, env) in [
        (true, vec![]),
        (false, vec![("RAILWAY_NO_AUTO_UPDATE", "1")]),
        (false, vec![("CI", "true")]),
    ] {
        let home = tempfile::tempdir().unwrap();
        seed_pending_updates(home.path());
        // Make checks and a detached sync genuinely due, so opt-outs cannot
        // accidentally pass just because the cache is still fresh.
        let skills_path = home.path().join(".railway/skills.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&skills_path).unwrap()).unwrap();
        manifest["cli_version"] = json!("0.0.0");
        manifest["last_checked"] = json!("2000-01-01T00:00:00Z");
        std::fs::write(skills_path, manifest.to_string()).unwrap();
        std::fs::write(
            home.path().join(".railway/version.json"),
            json!({"latest_version": "255.255.255"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            home.path().join(".railway/preferences.json"),
            json!({"autoUpdateDisabled": preference}).to_string(),
        )
        .unwrap();
        let version_before = std::fs::read(home.path().join(".railway/version.json")).unwrap();
        let skills_before = std::fs::read(home.path().join(".railway/skills.json")).unwrap();

        for args in [&["telemetry", "status"][..], &["--help"][..]] {
            let output = run_cli(home.path(), args, &env);
            for notice in [
                "New version available",
                "Railway skills update",
                "Unmanaged Railway skills",
                "railway skills update",
                "automatic update pending",
            ] {
                assert!(!output.contains(notice), "unexpected {notice}: {output}");
            }
        }
        // A detached updater launched before the opt-out must honor it too.
        let mut child_env = env.clone();
        child_env.push(("_RAILWAY_UPDATE_SKILLS", "1"));
        assert!(run_cli(home.path(), &[], &child_env).is_empty());
        assert_eq!(
            std::fs::read(home.path().join(".railway/version.json")).unwrap(),
            version_before
        );
        assert_eq!(
            std::fs::read(home.path().join(".railway/skills.json")).unwrap(),
            skills_before
        );
        assert!(!home.path().join(".railway/auto-update.log").exists());
    }
}

#[test]
fn manual_install_notices_are_once_per_release_and_skill_details_are_explicit() {
    let home = tempfile::tempdir().unwrap();
    seed_pending_updates(home.path());
    // Help/version paths do not consume notices.
    let output = run_cli(home.path(), &["--version"], &[]);
    assert!(!output.contains("New version available"), "{output}");
    let output = run_cli(home.path(), &["telemetry", "status"], &[]);
    assert!(
        output.contains("New version available: v255.255.255"),
        "{output}"
    );
    assert!(!output.contains("Railway skills update"), "{output}");
    let output = run_cli(home.path(), &["telemetry", "status"], &[]);
    assert!(!output.contains("New version available"), "{output}");
    let output = run_cli(home.path(), &["autoupdate", "status"], &[]);
    assert!(output.contains("Agent skills preserved"), "{output}");
    assert!(output.contains("Unmanaged Railway skills"), "{output}");
}

fn seed_completed_update(home: &Path, outcome: serde_json::Value) {
    seed_pending_updates(home);
    let path = home.join(".railway/skills.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["source_sha"] = json!("new");
    std::fs::write(path, manifest.to_string()).unwrap();
    std::fs::write(
        home.join(".railway/version.json"),
        json!({"last_update_check": chrono::Utc::now()}).to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join(".railway/update-status.json"),
        json!({
            "last_seen_version": env!("CARGO_PKG_VERSION"),
            "installed_version": env!("CARGO_PKG_VERSION"),
            "skills": {"cli_version": env!("CARGO_PKG_VERSION"), "outcome": outcome}
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn completion_receipt_is_once_only_and_is_not_consumed_by_pipes_help_or_json() {
    let home = tempfile::tempdir().unwrap();
    seed_completed_update(home.path(), json!({"status": "synced", "revision": "new"}));
    let piped = std::process::Command::new(env!("CARGO_BIN_EXE_railway"))
        .env_clear()
        .env("HOME", home.path())
        .env("DO_NOT_TRACK", "1")
        .args(["telemetry", "status"])
        .output()
        .unwrap();
    assert!(piped.status.success());
    assert!(!String::from_utf8_lossy(&piped.stderr).contains("Railway updated"));
    let help = run_cli(home.path(), &["--version"], &[]);
    assert!(!help.contains("Railway updated"));
    // Auth failure is expected with an empty HOME, but the valid JSON invocation
    // must still leave the receipt for an interactive, human-facing command.
    let (json, success) = run_cli_result(home.path(), &["status", "--json"], &[]);
    assert!(!success);
    assert!(!json.contains("Railway updated"), "{json}");
    let first = run_cli(home.path(), &["telemetry", "status"], &[]);
    assert!(
        first.contains(&format!(
            "Railway updated to v{} · agent skills synchronized",
            env!("CARGO_PKG_VERSION")
        )),
        "{first}"
    );
    let second = run_cli(home.path(), &["telemetry", "status"], &[]);
    assert!(!second.contains("Railway updated"), "{second}");
}

#[test]
fn receipts_describe_preserved_or_failed_skills_without_claiming_sync_success() {
    for (outcome, expected) in [
        (
            json!({"status": "preserved", "revision": "new", "count": 1}),
            "1 agent skill preserved (local edits)",
        ),
        (
            json!({"status": "failed", "message": "offline"}),
            "agent skills sync incomplete",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        seed_completed_update(home.path(), outcome);
        let output = run_cli(home.path(), &["telemetry", "status"], &[]);
        assert!(output.contains(expected), "{output}");
        assert!(!output.contains("skills synchronized"), "{output}");
        assert!(!output.contains("--force"), "{output}");
    }
}

#[test]
fn disabled_updates_do_not_display_or_consume_completion_receipts() {
    let home = tempfile::tempdir().unwrap();
    seed_completed_update(home.path(), json!({"status": "synced", "revision": "new"}));
    let output = run_cli(
        home.path(),
        &["telemetry", "status"],
        &[("RAILWAY_NO_AUTO_UPDATE", "1")],
    );
    assert!(!output.contains("Railway updated"), "{output}");
    let output = run_cli(home.path(), &["telemetry", "status"], &[]);
    assert!(output.contains("Railway updated"), "{output}");
}
