//! Exercise the real CLI against a local API with no customer credentials or resources.
#![cfg(all(unix, debug_assertions))]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct State {
    owners: Value,
    etag: String,
    requests: Vec<(Value, String)>,
    mutation_error: Option<Value>,
    snapshot_error: Option<Value>,
    stale_after_preview: bool,
    missing_etag: bool,
}

struct Backboard {
    url: String,
    state: Arc<Mutex<State>>,
}

impl Backboard {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/graphql/v2", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            owners: json!({"service.api": "legacy-ops", "service.admin": "legacy-ops", "bucket.media": "operations"}),
            etag: "reviewed-etag".into(),
            requests: Vec::new(),
            mutation_error: None,
            snapshot_error: None,
            stale_after_preview: false,
            missing_etag: false,
        }));
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut headers = String::new();
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
                    headers.push_str(&line);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                let mut state = shared.lock().unwrap();
                state.requests.push((request.clone(), headers));
                let query = request["query"].as_str().unwrap();
                let vars = &request["variables"];
                let response = if request["operationName"] == "ProjectToken" {
                    json!({"data":{"projectToken":{"id":"token","project":{"id":"project","name":"Project"},"environment":{"id":"env","name":"production"}}}})
                } else if query.contains("query IacPartialOwnership") || query.contains("query IacEnvironmentConfig") {
                    if let Some(error) = &state.snapshot_error {
                        json!({"errors":[error]})
                    } else {
                        let mut environment = json!({"id":"env", "name":"production", "projectId":"project", "config":{}, "configEtag":state.etag, "iacPartials":state.owners});
                        if state.missing_etag { environment["configEtag"] = Value::Null; }
                        if state.stale_after_preview { state.etag = "changed-etag".into(); }
                        json!({"data":{"environment":environment}})
                    }
                } else if query.contains("mutation IacPartial") {
                    if let Some(error) = &state.mutation_error {
                        json!({"errors":[error]})
                    } else if vars["baseConfigEtag"] != state.etag {
                        json!({"errors":[{"message":"The environment changed since this plan was computed. Re-run plan and review the changes before applying.","extensions":{"code":"STALE_ENVIRONMENT_BASE"}}]})
                    } else {
                        // Both operations must use the exact previewed set and the real contract.
                        let addresses = vars["resources"].as_array().unwrap();
                        assert!(!addresses.is_empty());
                        let source = if query.contains("environmentIacPartialTransfer") { &vars["fromPartial"] } else { &vars["partial"] };
                        for address in addresses {
                            assert_eq!(&state.owners[address.as_str().unwrap()], source);
                        }
                        for address in addresses {
                            if vars.get("toPartial").is_some() {
                                state.owners[address.as_str().unwrap()] = vars["toPartial"].clone();
                            } else {
                                state.owners.as_object_mut().unwrap().remove(address.as_str().unwrap());
                            }
                        }
                        state.etag = format!("{}-next", state.etag);
                        json!({"data":{"result":{"affectedResources":addresses,"iacPartials":state.owners}}})
                    }
                } else {
                    panic!("Unexpected request (resource changes are forbidden): {request}");
                }.to_string();
                drop(state);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
        });
        Self { url, state }
    }

    fn command(&self, home: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_railway"));
        command
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .env("RAILWAY_API_TOKEN", "test-token")
            .env("RAILWAY_PROJECT_ID", "project")
            .env("RAILWAY_ENVIRONMENT_ID", "env")
            .env("RAILWAY_BACKBOARD_URL", &self.url)
            .env("RAILWAY_NO_AUTO_UPDATE", "1")
            .env("DO_NOT_TRACK", "1")
            .env("NO_COLOR", "1")
            .env("CI", "true")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .current_dir(home);
        command
    }

    fn run(&self, home: &Path, args: &[&str]) -> Output {
        self.command(home)
            .args(["config", "partials"])
            .args(args)
            .output()
            .unwrap()
    }

    fn mutations(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(r, _)| r["query"].as_str().unwrap().contains("mutation"))
            .map(|(r, _)| r.clone())
            .collect()
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn failure(output: Output, message: &str) {
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(message),
        "expected {message:?}, got {stderr}"
    );
}

#[test]
fn discovers_orphaned_partials_in_human_and_json_output() {
    let server = Backboard::new();
    let home = tempfile::tempdir().unwrap();
    let text = success(server.run(home.path(), &["list"]));
    for expected in [
        "production (env)",
        "legacy-ops",
        "service.api",
        "service.admin",
        "bucket.media",
        "Named ownership remains",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    let output: Value =
        serde_json::from_str(&success(server.run(home.path(), &["list", "--json"]))).unwrap();
    assert_eq!(output["iacPartials"], server.state.lock().unwrap().owners);
    assert_eq!(output["configEtag"], "reviewed-etag");
    assert_eq!(output["wholeProjectAvailable"], false);
    assert!(server.mutations().is_empty());
    assert!(!home.path().join(".railway/railway.ts").exists());
}

#[test]
fn selected_transfer_and_whole_release_preserve_resources_and_files() {
    let server = Backboard::new();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".railway")).unwrap();
    let file = home.path().join(".railway/railway.ts");
    std::fs::write(&file, "authoring file must not be evaluated or edited").unwrap();
    let args = [
        "transfer",
        "legacy-ops",
        "operations",
        "--resource",
        "service.api",
        "--resource",
        "service.admin",
    ];
    let preview: Value = serde_json::from_str(&success(server.run(
        home.path(),
        &[args.as_slice(), &["--dry-run", "--json"]].concat(),
    )))
    .unwrap();
    assert_eq!(
        preview["affectedResources"],
        json!(["service.admin", "service.api"])
    );
    assert_eq!(preview["dryRun"], true);
    assert_eq!(preview["iacPartials"]["bucket.media"], "operations");
    assert!(server.mutations().is_empty());
    let result: Value = serde_json::from_str(&success(
        server.run(
            home.path(),
            &[
                args.as_slice(),
                &["--base-config-etag", "reviewed-etag", "--json"],
            ]
            .concat(),
        ),
    ))
    .unwrap();
    assert_eq!(result["iacPartials"], preview["iacPartials"]);
    assert_eq!(result["dryRun"], false);
    let transfer = &server.mutations()[0];
    assert_eq!(
        transfer["variables"],
        json!({"environmentId":"env","fromPartial":"legacy-ops","toPartial":"operations","resources":["service.admin","service.api"],"baseConfigEtag":"reviewed-etag"})
    );
    assert!(
        transfer["query"]
            .as_str()
            .unwrap()
            .contains("environmentIacPartialTransfer(")
    );
    let text = success(server.run(home.path(), &["release", "operations", "--yes"]));
    for expected in [
        "service.api",
        "service.admin",
        "bucket.media",
        "Resources are not changed, deleted, or redeployed",
        "whole-project planning is available",
        "can reclaim released ownership",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    let mutations = server.mutations();
    assert_eq!(mutations.len(), 2);
    assert_eq!(
        mutations[1]["variables"],
        json!({"environmentId":"env","partial":"operations","resources":["bucket.media","service.admin","service.api"],"baseConfigEtag":"reviewed-etag-next"})
    );
    assert!(
        server
            .state
            .lock()
            .unwrap()
            .owners
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        "authoring file must not be evaluated or edited"
    );
}

#[test]
fn selected_release_and_whole_transfer_support_new_destination() {
    let server = Backboard::new();
    let home = tempfile::tempdir().unwrap();
    let result: Value = serde_json::from_str(&success(server.run(
        home.path(),
        &[
            "release",
            "legacy-ops",
            "--resource",
            "service.api",
            "--json",
        ],
    )))
    .unwrap();
    assert_eq!(result["affectedResources"], json!(["service.api"]));
    assert_eq!(result["iacPartials"]["service.admin"], "legacy-ops");
    assert_eq!(result["wholeProjectAvailable"], false);
    let result: Value = serde_json::from_str(&success(server.run(
        home.path(),
        &["transfer", "legacy-ops", "new-partial", "--json"],
    )))
    .unwrap();
    assert_eq!(result["affectedResources"], json!(["service.admin"]));
    assert_eq!(result["iacPartials"]["service.admin"], "new-partial");
    assert_eq!(result["iacPartials"]["bucket.media"], "operations");
}

#[test]
fn noninteractive_changes_require_confirmation_but_dry_run_is_read_only() {
    let server = Backboard::new();
    let home = tempfile::tempdir().unwrap();
    failure(server.run(home.path(), &["release", "operations"]), "--yes");
    assert!(server.state.lock().unwrap().requests.is_empty());
    let text = success(server.run(home.path(), &["release", "operations", "--dry-run"]));
    assert!(text.contains("bucket.media"));
    assert!(text.contains("Dry run: no ownership changed"));
    assert!(server.mutations().is_empty());
}

#[test]
fn interactive_confirmation_defaults_to_no_and_applies_only_after_yes() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::time::{Duration, Instant};

    for (answer, expected_success) in [("\r", false), ("y\r", true)] {
        let server = Backboard::new();
        let home = tempfile::tempdir().unwrap();
        let base = server.command(home.path());
        let mut command = CommandBuilder::new(base.get_program());
        command.env_clear();
        for (key, value) in base.get_envs() {
            if let Some(value) = value {
                command.env(key, value);
            }
        }
        command.env_remove("CI");
        command.env("TERM", "xterm-256color");
        command.cwd(home.path());
        command.args(["config", "partials", "release", "operations"]);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 40,
                cols: 160,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(size) = reader.read(&mut buffer) {
                if size == 0
                    || sender
                        .send(String::from_utf8_lossy(&buffer[..size]).into_owned())
                        .is_err()
                {
                    break;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut output = String::new();
        let mut answered = false;
        loop {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("confirmation timed out: {output}");
            }
            if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(50)) {
                if chunk.contains("\x1b[6n") {
                    writer.write_all(b"\x1b[1;1R").unwrap();
                }
                output.push_str(&chunk);
                if !answered && output.contains("Change this ownership?") {
                    assert!(output.contains("bucket.media"));
                    assert!(output.contains("Resources are not changed, deleted, or redeployed"));
                    assert!(
                        server.mutations().is_empty(),
                        "preview must precede mutation"
                    );
                    writer.write_all(answer.as_bytes()).unwrap();
                    answered = true;
                }
            }
            if let Some(status) = child.try_wait().unwrap() {
                assert!(answered, "command exited before prompting: {output}");
                assert_eq!(status.success(), expected_success, "{output}");
                break;
            }
        }
        assert_eq!(server.mutations().len(), usize::from(expected_success));
    }
}

#[test]
fn rejects_invalid_sources_destinations_and_selections_without_mutation() {
    let server = Backboard::new();
    let home = tempfile::tempdir().unwrap();
    for (args, expected) in [
        (vec!["release", "missing", "--json"], "No resources"),
        (vec!["release", "*", "--json"], "Invalid partial"),
        (vec!["release", "", "--json"], "named IaC partial"),
        (
            vec!["transfer", "legacy-ops", "bad/name", "--json"],
            "Invalid partial",
        ),
        (
            vec!["transfer", "legacy-ops", "legacy-ops", "--json"],
            "must differ",
        ),
        (
            vec![
                "release",
                "legacy-ops",
                "--resource",
                "service.api",
                "--resource",
                "bucket.media",
                "--json",
            ],
            "not owned",
        ),
        (
            vec![
                "release",
                "legacy-ops",
                "--resource",
                "service.absent",
                "--json",
            ],
            "not owned",
        ),
        (
            vec!["release", "legacy-ops", "--resource", "", "--json"],
            "not owned",
        ),
        (
            vec!["release", "legacy-ops", "--resource", "--json"],
            "value is required",
        ),
    ] {
        failure(server.run(home.path(), &args), expected);
    }
    assert!(server.mutations().is_empty());
    assert_eq!(
        server.state.lock().unwrap().owners["service.api"],
        "legacy-ops"
    );
}

#[test]
fn rejects_stale_separate_review_and_state_changes_during_execution() {
    let server = Backboard::new();
    let home = tempfile::tempdir().unwrap();
    failure(
        server.run(
            home.path(),
            &[
                "release",
                "operations",
                "--base-config-etag",
                "old",
                "--json",
            ],
        ),
        "changed since this ownership preview",
    );
    assert!(server.mutations().is_empty());
    server.state.lock().unwrap().stale_after_preview = true;
    failure(
        server.run(home.path(), &["release", "operations", "--json"]),
        "changed since this plan",
    );
    assert_eq!(
        server.mutations().len(),
        1,
        "stale ownership mutations must not retry"
    );
    assert_eq!(
        server.mutations()[0]["variables"]["baseConfigEtag"],
        "reviewed-etag"
    );
    assert_eq!(
        server.state.lock().unwrap().owners["bucket.media"],
        "operations"
    );
}

#[test]
fn api_errors_and_missing_snapshot_data_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    for error in [
        "Cannot query field \"environmentIacPartialRelease\" on type \"Mutation\".",
        "ADMIN access required",
        "IaC ownership changed while this operation was running.",
    ] {
        let server = Backboard::new();
        server.state.lock().unwrap().mutation_error = Some(json!({"message":error}));
        failure(
            server.run(home.path(), &["release", "operations", "--json"]),
            error,
        );
        assert_eq!(server.mutations().len(), 1);
        assert_eq!(
            server.state.lock().unwrap().owners["bucket.media"],
            "operations"
        );
    }
    let server = Backboard::new();
    server.state.lock().unwrap().snapshot_error =
        Some(json!({"message":"Cannot query field iacPartials"}));
    failure(
        server.run(home.path(), &["release", "operations", "--json"]),
        "iacPartials",
    );
    assert_eq!(
        server.state.lock().unwrap().requests.len(),
        1,
        "no ownership-less fallback"
    );
    let server = Backboard::new();
    server.state.lock().unwrap().missing_etag = true;
    failure(
        server.run(home.path(), &["release", "operations", "--json"]),
        "Could not read IaC partial ownership",
    );
    assert!(server.mutations().is_empty());
}

#[test]
fn uses_linked_environment_and_environment_scoped_project_token() {
    let server = Backboard::new();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".railway")).unwrap();
    let canonical_home = home.path().canonicalize().unwrap();
    let path = canonical_home.to_str().unwrap();
    std::fs::write(home.path().join(".railway/config.json"), json!({"projects":{path:{"projectPath":path,"project":"project","environment":"env"}},"user":{}}).to_string()).unwrap();
    success(
        server
            .command(home.path())
            .env_remove("RAILWAY_PROJECT_ID")
            .env_remove("RAILWAY_ENVIRONMENT_ID")
            .args(["config", "partials", "list", "--json"])
            .output()
            .unwrap(),
    );
    assert!(
        server.state.lock().unwrap().requests[0]
            .1
            .contains("authorization: Bearer test-token")
    );
    success(
        server
            .command(home.path())
            .env_remove("RAILWAY_API_TOKEN")
            .env_remove("RAILWAY_PROJECT_ID")
            .env_remove("RAILWAY_ENVIRONMENT_ID")
            .env("RAILWAY_TOKEN", "scoped-project-token")
            .args(["config", "partials", "release", "operations", "--json"])
            .output()
            .unwrap(),
    );
    let state = server.state.lock().unwrap();
    for (request, headers) in &state.requests[1..] {
        assert!(headers.contains("project-access-token: scoped-project-token"));
        assert!(!headers.contains("authorization:"));
        if request["operationName"] != "ProjectToken" {
            assert_eq!(request["variables"]["environmentId"], "env");
        }
    }
}

#[test]
fn ownership_changes_invalidate_existing_saved_configuration_plans() {
    let server = Backboard::new();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".railway")).unwrap();
    std::fs::write(home.path().join(".railway/railway.ts"), "do not evaluate").unwrap();
    // Match the existing saved-plan canonical hash and source-tree conventions.
    let source_tree = format!(
        "sha256:{:x}",
        Sha256::digest(b"railway.ts\0do not evaluate\0")
    );
    let changes = json!({"changes":[],"version":1});
    let hash = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&changes).unwrap())
    );
    let plan_path = home.path().join("plan.json");
    std::fs::write(&plan_path, json!({"kind":"railway.config.plan","version":1,"cliVersion":"5.58.0","sourceTree":source_tree,"environmentId":"env","configEtag":"reviewed-etag","changeSetHash":hash,"changeSet":changes,"destructive":false}).to_string()).unwrap();
    success(server.run(home.path(), &["release", "operations", "--json"]));
    failure(
        server
            .command(home.path())
            .args([
                "config",
                "apply",
                "--plan",
                plan_path.to_str().unwrap(),
                "--yes",
            ])
            .output()
            .unwrap(),
        "changed since this plan",
    );
    assert_eq!(
        server.mutations().len(),
        1,
        "saved config apply must stop before mutation"
    );
}
