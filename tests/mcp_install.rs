use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(home: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_railway"));
    command
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("DO_NOT_TRACK", "1")
        .env("NO_COLOR", "1")
        .env("RAILWAY_AGENT_ADVISORY", "0")
        .env("RAILWAY_NO_AUTO_UPDATE", "1")
        .args(["mcp", "install", "--agent", "codex"]);
    command.output().unwrap()
}

#[test]
fn malformed_codex_config_fails_without_claiming_success() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".codex/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = b"[[ invalid TOML\n";
    fs::write(&path, original).unwrap();

    let output = run(home.path());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(
        !output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("OpenAI Codex: failed"), "stdout: {stdout}");
    assert!(
        !stdout.contains("MCP server installed successfully!"),
        "stdout: {stdout}"
    );
    assert!(
        stderr.contains("Railway MCP server installation failed for OpenAI Codex"),
        "stderr: {stderr}"
    );
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn valid_codex_config_still_installs_and_preserves_existing_settings() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".codex/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "model = \"fixture-model\"\n").unwrap();

    let output = run(home.path());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("MCP server installed successfully!"));

    let document = fs::read_to_string(path)
        .unwrap()
        .parse::<toml::Value>()
        .unwrap();
    assert_eq!(
        document.get("model").and_then(toml::Value::as_str),
        Some("fixture-model")
    );
    let railway = document
        .get("mcp_servers")
        .and_then(|servers| servers.get("railway"))
        .unwrap();
    assert_eq!(
        railway.get("command").and_then(toml::Value::as_str),
        Some("railway")
    );
    assert_eq!(
        railway
            .get("args")
            .and_then(toml::Value::as_array)
            .and_then(|args| args.first())
            .and_then(toml::Value::as_str),
        Some("mcp")
    );
}
