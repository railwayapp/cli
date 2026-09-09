//! Replay connection snapshots with an isolated home, no login, and no network.
#![cfg(unix)]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::{Value, json};

fn fixture(home: &Path, beta: bool) -> Value {
    let saved = json!({
        "version": 1,
        "saved_at": "2026-09-09T12:00:00Z",
        "agent_id": "agent-123",
        "agent_name": "my-box",
        "environment_id": "env-123",
        "harness": if beta { "opencode2" } else { "opencode" },
        "ssh_command": "ssh agent:env-123:agent-123@ssh.railway.com",
        "ssh_config": "Host railway-agent-my-box\n    HostName ssh.railway.com\n    User agent:env-123:agent-123\n",
        "opencode": {
            "connection": {
                "url": "https://example.up.railway.app",
                "username": "opencode",
                "password": "fixture-password",
                "directory": "/app/my project",
                "reused": true
            },
            "beta": beta,
            "desktop_configured": true
        }
    });
    fs::create_dir_all(home.join(".railway")).unwrap();
    fs::write(
        home.join(".railway/last-code-config.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    saved
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_railway"))
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("DO_NOT_TRACK", "1")
        .env("NO_COLOR", "1")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        // Leave automatic updates enabled: get-config must skip them itself.
        .current_dir(home)
        .args(["code", "get-config"])
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn get_config_reports_how_to_save_a_first_connection() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No saved connection details yet"));
    assert!(output.stdout.is_empty());
    assert!(!home.path().join(".railway/last-code-config.json").exists());
}

#[test]
fn get_config_replays_both_editions_and_ssh_without_prompting() {
    for beta in [false, true] {
        let home = tempfile::tempdir().unwrap();
        fixture(home.path(), beta);
        let output = run(home.path(), &[]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        let edition = if beta { "OpenCode2 [Beta]" } else { "OpenCode" };
        assert_eq!(
            text.matches(&format!("Railway {edition} Server Configuration:"))
                .count(),
            1
        );
        assert_eq!(
            text.matches(&"─".repeat(64)).count(),
            2,
            "one results panel"
        );
        assert_eq!(text.matches("Connect with the Railway CLI:").count(), 1);
        assert_eq!(
            text.matches("Railway Cloud Agent SSH Configuration:")
                .count(),
            1
        );
        let flag = if beta { "--opencode2" } else { "--opencode" };
        assert_eq!(
            text.matches(&format!("railway code {flag} connect my-box"))
                .count(),
            1
        );
        assert_eq!(text.matches("railway ca ssh agent-123").count(), 1);
        assert_eq!(
            text.matches("OpenCode Desktop configuration updated")
                .count(),
            1
        );
        for expected in [
            "https://example.up.railway.app",
            "fixture-password",
            "/app/my project",
            "OpenCode Desktop configuration updated (you may need to restart)",
            "Railway Cloud Agent SSH Configuration:",
            "Host railway-agent-my-box",
            "ssh agent:env-123:agent-123@ssh.railway.com",
            "railway ca ssh agent-123",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("Launch "));
        assert!(!text.contains('\x1b'));
    }
}

#[test]
fn get_config_json_is_exact_and_does_not_read_login_or_modify_snapshot() {
    let home = tempfile::tempdir().unwrap();
    let expected = fixture(home.path(), true);
    fs::write(
        home.path().join(".railway/config.json"),
        "invalid login config",
    )
    .unwrap();
    let path = home.path().join(".railway/last-code-config.json");
    let before = fs::read(&path).unwrap();
    let output = run(home.path(), &["--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        expected
    );
    assert_eq!(fs::read(path).unwrap(), before);
    assert_eq!(
        fs::read_dir(home.path().join(".railway")).unwrap().count(),
        2
    );
}

#[test]
fn get_config_rejects_corrupt_or_future_snapshots() {
    let home = tempfile::tempdir().unwrap();
    let mut saved = fixture(home.path(), false);
    let path = home.path().join(".railway/last-code-config.json");
    saved["version"] = json!(99);
    fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let output = run(home.path(), &[]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Unsupported saved Railway code configuration version")
    );
    assert!(output.stdout.is_empty());
    fs::write(&path, "{").unwrap();
    let output = run(home.path(), &[]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Invalid saved Railway code configuration")
    );
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(&path).unwrap(), b"{");
}
