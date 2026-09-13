//! Check command-specific help through the same command builder used by the CLI.
use clap::error::ErrorKind;

fn help(args: &[&str], flag: &str) -> String {
    let error = crate::build_args()
        .try_get_matches_from(
            std::iter::once("railway")
                .chain(args.iter().copied())
                .chain(std::iter::once(flag)),
        )
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::DisplayHelp);
    error
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn launcher_help_matches_each_execution_path() {
    for flag in ["-h", "--help"] {
        let code = help(&["code"], flag);
        assert!(code.contains("railway code --codex connect my-box"));
        assert!(code.contains("--connection-json"));
        assert!(code.contains("command approvals disabled"));
        assert!(code.contains("An explicit agent flag creates a new VM"));
        for args in [vec!["ca"], vec!["ca", "start"]] {
            let text = help(&args, flag);
            assert!(!text.contains("railway code --codex connect"));
            assert!(!text.contains("--connection-json"));
            assert!(!text.contains("desktop-only"));
            assert!(!text.contains("--agent <NAME_OR_ID>"));
            assert!(text.contains("Arguments to pass to the agent after --"));
            assert!(text.contains("VM running"));
        }
    }
}

#[test]
fn focused_help_retains_side_effects_and_workflow_examples() {
    for (args, expected) in [
        (
            vec!["ca", "desktop"],
            vec![
                "--remove",
                "stops the managed OpenCode server",
                "VM stays running",
            ],
        ),
        (
            vec!["ca", "bootstrap"],
            vec!["bootstrap save dev", "saved on this machine"],
        ),
        (
            vec!["ca", "bootstrap", "default"],
            vec!["Name of a ready bootstrap"],
        ),
        (
            vec!["ca", "create"],
            vec![
                "--from-checkpoint <checkpoint-id>",
                "cloud-agent checkpoint ID",
            ],
        ),
        (
            vec!["ca", "ssh"],
            vec!["--session", "--resume", "Sleep ends its processes"],
        ),
        (
            vec!["code", "get-config"],
            vec![
                "without login or network access",
                "Snapshots include credentials",
            ],
        ),
        (
            vec!["sandbox", "create"],
            vec!["--private-network --domain 3000"],
        ),
        (
            vec!["sandbox", "fork"],
            vec!["are not inherited", "--domain web:3000"],
        ),
    ] {
        let text = help(&args, "--help");
        for expected in expected {
            assert!(text.contains(expected), "{args:?} missing {expected:?}");
        }
    }
}

#[test]
fn documented_examples_parse_without_executing_commands() {
    for args in [
        vec!["code", "--codex"],
        vec!["code", "--claude"],
        vec!["code", "--codex", "connect", "my-box"],
        vec!["code", "--codex", "desktop-only"],
        vec!["code", "get-config", "my-box"],
        vec!["ca", "start", "--codex", "--new"],
        // Help visibility must not remove previously accepted options.
        vec!["ca", "--codex", "--connection-json"],
        vec![
            "ca", "start", "--codex", "--agent", "my-box", "--dir", "/app",
        ],
        vec![
            "ca",
            "start",
            "--claude",
            "--",
            "exec",
            "explain this codebase",
        ],
        vec!["ca", "desktop", "--opencode", "--agent", "my-box"],
        vec![
            "ca",
            "bootstrap",
            "save",
            "dev",
            "--agent",
            "my-box",
            "--default",
        ],
        vec!["ca", "bootstrap", "default", "dev"],
        vec!["code", "--codex", "--bootstrap", "dev"],
        vec![
            "ca",
            "create",
            "my-box",
            "--from-checkpoint",
            "checkpoint-id",
        ],
        vec!["ca", "ssh", "my-box", "--session"],
        vec!["ca", "ssh", "my-box", "--resume"],
        vec!["sandbox", "create", "--private-network", "--domain", "3000"],
        vec![
            "sandbox",
            "fork",
            "sandbox-id",
            "--private-network",
            "--domain",
            "web:3000",
        ],
        vec!["sandbox", "exec", "--detach", "--", "npm", "run", "build"],
        vec!["sandbox", "forward", "8080:3000"],
    ] {
        crate::build_args()
            .try_get_matches_from(std::iter::once("railway").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("{args:?}: {error}"));
    }
}
