//! Exercise the actual CLI parser/dispatch using isolated homes and no login.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Command, Output},
};

fn fixture(home: &Path, harness: &str, id: &str, name: &str) -> Value {
    let mut value = json!({"version":1,"saved_at":"2026-09-09T12:00:00Z",
        "agent_id":id,"agent_name":name,"environment_id":"env-123","harness":harness,
        "ssh_command":"ssh agent:env-123:agent-123@ssh.railway.com",
        "ssh_config":"Host railway-agent-my-box\n    HostName ssh.railway.com\n    User agent:env-123:agent-123\n"});
    if harness == "codex" {
        value["codex"] = json!({"connection":{"url":"wss://example.up.railway.app:443","token":"fixture-token",
            "directory":"/app/my project","version":"0.153.4","reused":true},"desktop_only":true,
            "desktop":{"ssh_alias":"custom-ssh","ssh_config_path":"/home/user/.ssh/config",
                "config_path":"/home/user/.codex/codex-app/config.json","project_label":"My Codex Project",
                "remote_path":"/app/my project","apply_url":"codex://codex-app/apply-config","apply_sent":true}});
    } else if harness.starts_with("opencode") {
        value["opencode"] = json!({"connection":{"url":"https://example.up.railway.app","username":"opencode",
            "password":"fixture-password","directory":"/app/my project","reused":true},
            "beta":harness=="opencode2","desktop_configured":true});
    }
    fs::create_dir_all(home.join(".railway")).unwrap();
    fs::write(
        home.join(".railway/last-code-config.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    value
}

fn run(home: &Path, args: &[&str]) -> Output {
    // Keep updates and telemetry enabled: offline replay must skip both itself.
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let address = format!("http://{}", proxy.local_addr().unwrap());
    let output = Command::new(env!("CARGO_BIN_EXE_railway"))
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("NO_COLOR", "1")
        .env("HTTP_PROXY", &address)
        .env("HTTPS_PROXY", &address)
        .env("ALL_PROXY", &address)
        .current_dir(home)
        .args(["code", "get-config"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        matches!(proxy.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "get-config attempted network access"
    );
    output
}

#[test]
fn replay_supports_codex_both_opencode_editions_and_generic_ssh_in_one_panel() {
    for harness in ["codex", "opencode", "opencode2", "claude"] {
        let home = tempfile::tempdir().unwrap();
        fixture(home.path(), harness, "agent-123", "my-box");
        let output = run(home.path(), &["my-box"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.matches(&"─".repeat(64)).count(), 2);
        if harness != "codex" {
            assert_eq!(
                text.matches("Railway Cloud Agent SSH Configuration:")
                    .count(),
                1
            );
            assert!(text.contains("Host railway-agent-my-box"));
        }
        assert_eq!(text.matches("Connect with the Railway CLI:").count(), 1);
        assert!(!text.contains("Launch ") && !text.contains('\x1b'));
        if harness == "codex" {
            for expected in [
                "fixture-token",
                "Codex Desktop configured: My Codex Project",
                "SSH configuration written to /home/user/.ssh/config",
                "wss://example.up.railway.app:443",
                "Codex App Server Configuration:",
                "--remote-auth-token-env RAILWAY_CODEX_SERVER_TOKEN",
                "railway code --codex connect agent-123",
                "railway code get-config agent-123",
            ] {
                assert!(text.contains(expected), "missing {expected}");
            }
        } else if harness.starts_with("opencode") {
            assert!(
                text.contains("fixture-password") && text.contains("Desktop configuration updated")
            );
        }
    }
}

#[test]
fn json_uses_legacy_snapshots_without_login_or_writes() {
    for harness in ["codex", "opencode2"] {
        let home = tempfile::tempdir().unwrap();
        let expected = fixture(home.path(), harness, "agent-123", "my-box");
        fs::write(
            home.path().join(".railway/config.json"),
            "invalid login config",
        )
        .unwrap();
        let path = home.path().join(".railway/last-code-config.json");
        let before = fs::read(&path).unwrap();
        for args in [
            vec!["--json"],
            vec!["my-box", "--json"],
            vec!["--json", "agent-123"],
        ] {
            let output = run(home.path(), &args);
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
        }
        assert_eq!(fs::read(path).unwrap(), before);
        assert_eq!(
            fs::read_dir(home.path().join(".railway")).unwrap().count(),
            2
        );
    }
}

#[test]
fn named_lookup_selects_an_older_agent_and_reports_duplicates_and_missing_agents() {
    let home = tempfile::tempdir().unwrap();
    let mut first = fixture(home.path(), "codex", "first-id", "shared-name");
    first["saved_at"] = json!("2026-09-08T12:00:00Z");
    let second = fixture(home.path(), "opencode", "second-id", "shared-name");
    fs::write(
        home.path().join(".railway/code-configs.json"),
        serde_json::to_vec(&json!({"version":1,"connections":[first,second]})).unwrap(),
    )
    .unwrap();
    for (args, expected) in [
        (vec!["first-id", "--json"], &first),
        (vec!["--json"], &second),
    ] {
        let output = run(home.path(), &args);
        assert!(output.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            *expected
        );
    }
    for (selector, message) in [
        ("shared-name", "Multiple saved agents"),
        ("missing", "No saved connection details for"),
        ("../../etc/passwd", "No saved connection details for"),
    ] {
        let output = run(home.path(), &[selector]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(message));
        assert!(!error.contains("fixture-token") && !error.contains("fixture-password"));
    }
}

#[test]
fn empty_corrupt_and_future_snapshots_are_read_only_errors() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No saved connection details yet"));
    assert!(!home.path().join(".railway/last-code-config.json").exists());
    let mut saved = fixture(home.path(), "codex", "id", "box");
    saved["version"] = json!(99);
    let path = home.path().join(".railway/last-code-config.json");
    for contents in [serde_json::to_vec(&saved).unwrap(), b"{".to_vec()] {
        fs::write(&path, &contents).unwrap();
        let output = run(home.path(), &["--json"]);
        assert!(!output.status.success() && output.stdout.is_empty());
        assert_eq!(fs::read(&path).unwrap(), contents);
    }
}
