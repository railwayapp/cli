//! Exercise the actual CLI, output and exit codes against a local GraphQL server.
#![cfg(all(unix, debug_assertions))]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Command, Output},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};

struct Backboard {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Backboard {
    fn new(inventory: Vec<Value>, refused: Option<&str>, wake_status: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/graphql/v2", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let refused = refused.map(str::to_owned);
        let wake_status = wake_status.to_owned();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                seen.lock().unwrap().push(request.clone());
                let operation = request["operationName"].as_str().unwrap();
                let id = request["variables"]["id"].as_str().unwrap_or_default();
                if operation.starts_with("AgentBootstrap") {
                    assert!(
                        request_line.starts_with("POST /graphql/internal "),
                        "{request_line}"
                    );
                    assert!(
                        !request["query"]
                            .as_str()
                            .unwrap()
                            .contains("agentBootstrapDefault")
                    );
                }
                let response = match operation {
                    "Project" => {
                        let project = request["variables"]["id"].as_str().unwrap();
                        json!({"data": {"project": {
                            "id": project, "name": project, "workspaceId": "workspace", "deletedAt": null,
                            "workspace": {"name": "workspace"}, "buckets": {"edges": []}, "services": {"edges": []},
                            "environments": {"edges": [{"node": {
                                "id": format!("{project}-env"), "name": "production", "canAccess": true,
                                "deletedAt": null, "unmergedChangesCount": 0
                            }}]}
                        }}})
                    }
                    "AgentBootstraps" => {
                        let environment = request["variables"]["environmentId"].as_str().unwrap();
                        json!({"data": {"agentBootstraps": [{
                            "id": "bootstrap", "name": "dev", "environmentId": environment,
                            "status": "READY", "failureReason": null, "updatedAt": "2026-09-11T00:00:00Z"
                        }]}})
                    }
                    "MyCloudAgents" => json!({"data": {"myCloudAgents": inventory}}),
                    "CloudAgentSleep" | "CloudAgentWake" if refused.as_deref() == Some(id) => {
                        json!({"errors": [{"message": "mutation refused"}]})
                    }
                    "CloudAgentSleep" => json!({"data": {"cloudAgentSleep": {"id": id, "status": "RUNNING"}}}),
                    "CloudAgentWake" => json!({"data": {"cloudAgentWake": {"id": id, "status": "STARTING"}}}),
                    "CloudAgent" => json!({"data": {"cloudAgent": agent(id, &wake_status)}}),
                    _ => json!({"errors": [{"message": format!("unexpected operation: {operation}") }]}),
                }.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
        });
        Self { url, requests }
    }

    fn run(&self, home: &std::path::Path, args: &[&str]) -> Output {
        // No real credentials, keys or network routes are inherited. The absent
        // SSH key also exercises sleep's best-effort flush failure path.
        Command::new(env!("CARGO_BIN_EXE_railway"))
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .env("RAILWAY_API_TOKEN", "test-token")
            .env("RAILWAY_BACKBOARD_URL", &self.url)
            .env("RAILWAY_NO_AUTO_UPDATE", "1")
            .env("DO_NOT_TRACK", "1")
            .env("NO_COLOR", "1")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .current_dir(home)
            .args(["ca"])
            .args(args)
            .output()
            .unwrap()
    }

    fn ids_for(&self, operation: &str) -> Vec<String> {
        let mut ids: Vec<_> = self
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["operationName"] == operation)
            .map(|r| r["variables"]["id"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    }
}

fn agent(id: &str, status: &str) -> Value {
    json!({"id": id, "name": id, "status": status, "projectId": "project",
        "environmentId": "env", "createdAt": "2026-09-10T00:00:00Z"})
}

#[test]
fn sleep_skips_an_asleep_agent_and_sends_every_other_state() {
    for status in [
        "RUNNING",
        "STARTING",
        "SLEEPING",
        "FAILED",
        "CRASHED",
        "DELETING",
        "FUTURE_STATE",
    ] {
        // The status sleep reads before acting is the `CloudAgent` response.
        let server = Backboard::new(vec![agent("existing", status)], None, status);
        let home = tempfile::tempdir().unwrap();
        let output = server.run(home.path(), &["sleep", "existing"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        if status == "SLEEPING" {
            // Connecting to flush would wake it; nothing is sent.
            assert!(server.ids_for("CloudAgentSleep").is_empty(), "{status}");
            assert!(stdout.contains("already asleep"), "{stdout}");
        } else {
            assert_eq!(server.ids_for("CloudAgentSleep"), ["existing"], "{status}");
            assert!(stdout.contains("Sleep requested"), "{stdout}");
        }
        assert_eq!(
            server.ids_for("CloudAgent"),
            ["existing"],
            "one status read; acceptance does not poll for completion"
        );
    }
}

#[test]
fn bulk_sleep_attempts_every_running_agent_and_reports_partial_failure() {
    for refused in [None, Some("also-running")] {
        let server = Backboard::new(
            vec![
                agent("running", "RUNNING"),
                agent("also-running", "RUNNING"),
                agent("sleeping", "SLEEPING"),
                agent("unknown", "FUTURE_STATE"),
            ],
            refused,
            "RUNNING",
        );
        let home = tempfile::tempdir().unwrap();
        let output = server.run(home.path(), &["sleep", "--all"]);
        assert_eq!(output.status.success(), refused.is_none());
        assert_eq!(
            server.ids_for("CloudAgentSleep"),
            ["also-running", "running"]
        );
        let count = if refused.is_some() { 1 } else { 2 };
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains(&format!("Sleep requested for {count} agents"))
        );
        if refused.is_some() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("also-running — mutation refused"),
                "{stderr}"
            );
        }
    }
}

#[test]
fn bulk_sleep_with_nothing_running_sends_nothing() {
    let server = Backboard::new(
        vec![agent("sleeping", "SLEEPING"), agent("crashed", "CRASHED")],
        None,
        "SLEEPING",
    );
    let home = tempfile::tempdir().unwrap();
    let output = server.run(home.path(), &["sleep", "--all"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("No running cloud agents"));
    assert!(server.ids_for("CloudAgentSleep").is_empty());
    assert!(server.ids_for("CloudAgent").is_empty());
}

#[test]
fn single_sleep_refusal_is_not_reported_as_success() {
    let server = Backboard::new(
        vec![agent("existing", "SLEEPING")],
        Some("existing"),
        "RUNNING",
    );
    let home = tempfile::tempdir().unwrap();
    let output = server.run(home.path(), &["sleep", "existing"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Sleep requested"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("mutation refused"));
    assert_eq!(server.ids_for("CloudAgentSleep"), ["existing"]);
}

#[test]
fn wake_wait_reports_a_terminal_observation_and_no_wait_does_not_poll() {
    for no_wait in [false, true] {
        let server = Backboard::new(vec![agent("existing", "RUNNING")], None, "CRASHED");
        let home = tempfile::tempdir().unwrap();
        let args = if no_wait {
            vec!["wake", "existing", "--no-wait"]
        } else {
            vec!["wake", "existing"]
        };
        let output = server.run(home.path(), &args);
        assert_eq!(output.status.success(), no_wait);
        assert_eq!(server.ids_for("CloudAgentWake"), ["existing"]);
        assert_eq!(server.ids_for("CloudAgent").len(), usize::from(!no_wait));
        if !no_wait {
            assert!(String::from_utf8_lossy(&output.stderr).contains("crashed"));
            assert!(!String::from_utf8_lossy(&output.stdout).contains("is running"));
        }
    }
}

#[test]
fn missing_remembered_agent_stops_ssh_before_replacement_creation() {
    let server = Backboard::new(vec![], None, "RUNNING");
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".railway")).unwrap();
    std::fs::write(
        home.path().join(".railway/config.json"),
        json!({"projects": {}, "user": {}, "codeAgents": {"env": "remembered"}}).to_string(),
    )
    .unwrap();
    let output = server.run(home.path(), &["ssh"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("remembered"));
    assert!(server.ids_for("CloudAgentCreate").is_empty());
}

#[test]
fn bootstrap_list_uses_directory_link_before_preferences_and_flags_before_link() {
    let server = Backboard::new(vec![], None, "RUNNING");
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".railway");
    std::fs::create_dir_all(&config).unwrap();
    // `railway link` records current_dir(), which resolves macOS's /var and
    // /tmp symlinks. Match that path instead of the temporary directory alias.
    let linked_directory = home.path().canonicalize().unwrap();
    let path = linked_directory.to_str().unwrap();
    std::fs::write(config.join("config.json"), json!({
        "projects": {path: {"projectPath": path, "project": "linked", "environment": "linked-env"}},
        "user": {}
    }).to_string()).unwrap();
    std::fs::write(
        config.join("agent-prefs.json"),
        json!({
            "version": 1, "defaultProject": {"projectId": "preferred", "projectName": "preferred",
                "environmentId": "preferred-env", "environmentName": "production"}
        })
        .to_string(),
    )
    .unwrap();
    for (args, expected) in [
        (vec!["bootstrap", "list", "--json"], "linked-env"),
        (
            vec![
                "bootstrap",
                "list",
                "--json",
                "--project",
                "explicit",
                "--environment",
                "production",
            ],
            "explicit-env",
        ),
    ] {
        let output = server.run(home.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(rows[0]["name"], "dev");
        assert_eq!(rows[0]["environmentId"], expected);
        assert_eq!(rows[0]["isDefault"], false);
    }
    let output = server.run(home.path(), &["bootstrap", "default", "dev", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let selected: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(selected["isDefault"], true);
    for (args, expected_default) in [
        (vec!["bootstrap", "list", "--json"], true),
        (
            vec![
                "bootstrap",
                "list",
                "--json",
                "--project",
                "explicit",
                "--environment",
                "production",
            ],
            false,
        ),
    ] {
        let output = server.run(home.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(rows[0]["isDefault"], expected_default);
    }
    let stored: Value =
        serde_json::from_slice(&std::fs::read(config.join("config.json")).unwrap()).unwrap();
    assert_eq!(
        stored["agentBootstrapDefaults"]["railway.com:linked-env"],
        "bootstrap"
    );
    let requests = server.requests.lock().unwrap();
    assert!(!requests.iter().any(|r| r["variables"]["id"] == "preferred"));
}
