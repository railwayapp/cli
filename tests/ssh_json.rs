//! `railway ssh --json` reports failures before the connection as a JSON error on stdout.
#![cfg(unix)]

use std::process::Command;

use serde_json::Value;

#[test]
fn signed_out_ssh_json_prints_unauthorized_code() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_railway"))
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RAILWAY_NO_AUTO_UPDATE", "1")
        .env("DO_NOT_TRACK", "1")
        .env("NO_COLOR", "1")
        .env("CI", "true")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .current_dir(home.path())
        .args(["ssh", "--json", "--project", "project", "sh", "-c", "true"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let error: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(error["code"], "UNAUTHORIZED");
}
