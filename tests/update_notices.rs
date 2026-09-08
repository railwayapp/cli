//! Exercise startup with a real TTY: piped output would hide the banner even
//! without honoring the persisted auto-update preference.
#![cfg(unix)]

use std::io::Read;
use std::path::Path;

use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde_json::json;

fn run_cli(home: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
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
    assert!(child.wait().unwrap().success());
    output.join().unwrap()
}

fn seed_pending_updates(home: &Path) {
    std::fs::create_dir_all(home.join(".railway")).unwrap();
    std::fs::write(
        home.join(".railway/version.json"),
        json!({"latest_version": "255.255.255"}).to_string(),
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
fn enabled_updates_still_report_pending_cli_and_blocked_skills() {
    let home = tempfile::tempdir().unwrap();
    seed_pending_updates(home.path());
    // --version exercises the banners without starting a network check/install.
    let output = run_cli(home.path(), &["--version"], &[]);
    assert!(
        output.contains("New version available: v255.255.255"),
        "{output}"
    );
    assert!(
        output.contains("Railway skills update skipped due to local changes"),
        "{output}"
    );

    // A clean pending update is left to auto-apply, without a manual prompt.
    let path = home.path().join(".railway/skills.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["auto_applied_sha"] = serde_json::Value::Null;
    std::fs::write(path, manifest.to_string()).unwrap();
    let output = run_cli(home.path(), &["--version"], &[]);
    assert!(output.contains("New version available"), "{output}");
    assert!(!output.contains("Railway skills update"), "{output}");
}
