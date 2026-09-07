//! OpenCode Desktop connects to an HTTP server, not an OpenSSH host entry.
//! Run the server and its loopback tunnel together in the foreground. Keep a
//! reusable script as an optional shortcut, using the same SSH arguments.
//! The server is added through OpenCode's server picker: `opencode.json` has
//! no desktop connection setting, and its private app storage is not a config
//! API. See https://opencode.ai/docs/server/ and /docs/windows-wsl/.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::util::shell::{shell_join, shell_quote};

pub(super) fn script_path(home: &Path, agent_id: &str) -> PathBuf {
    // IDs come from the server. Hash them so they cannot become path traversal,
    // and so a rename or alias change still replaces the same agent's script.
    let id = format!("{:x}", Sha256::digest(agent_id.as_bytes()));
    home.join(".railway/desktop/opencode")
        .join(format!("{id}.sh"))
}

pub(super) fn check_local_port(port: u16) -> Result<()> {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).with_context(|| {
        format!("Cannot bind OpenCode's local port {port}. Stop the existing tunnel or choose another port with --port.")
    })?;
    Ok(())
}

fn ssh_args(alias: &str, dir: &str, ssh_config: &Path, port: u16, remote_port: u16) -> Vec<String> {
    let server = format!(
        "cd -- {} && exec opencode serve --hostname 127.0.0.1 --port {remote_port}",
        shell_quote(dir)
    );
    let remote = shell_join(&["bash".into(), "-lc".into(), server]);
    vec![
        "-F".into(),
        ssh_config.to_string_lossy().into_owned(),
        // A remote PTY lets Ctrl-C reach the foreground server.
        "-tt".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=20".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-L".into(),
        format!("127.0.0.1:{port}:127.0.0.1:{remote_port}"),
        "--".into(),
        alias.into(),
        remote,
    ]
}

pub(super) fn connection_command(
    alias: &str,
    dir: &str,
    ssh_config: &Path,
    port: u16,
    remote_port: u16,
) -> std::process::Command {
    let mut command = std::process::Command::new("ssh");
    command.args(ssh_args(alias, dir, ssh_config, port, remote_port));
    command
}

/// Like the other foreground SSH commands, inherit the terminal and wait for
/// the remote process. The PTY carries Ctrl-C to `opencode serve`.
pub(super) fn run(mut command: std::process::Command) -> Result<()> {
    let status = command
        .status()
        .context("Failed to start OpenCode's SSH connection")?;
    if !status.success() {
        bail!("OpenCode server or SSH tunnel exited with {status}");
    }
    Ok(())
}

pub(super) fn render_script(
    alias: &str,
    dir: &str,
    ssh_config: &Path,
    port: u16,
    remote_port: u16,
) -> String {
    let mut args = vec!["ssh".into()];
    args.extend(ssh_args(alias, dir, ssh_config, port, remote_port));
    let ssh = shell_join(&args);
    // Desktop 1.18.29 treats literal localhost/127.0.0.1 as local and selects
    // the computer's own folders. The absolute localhost name selects its
    // server-side picker while keeping loopback transport.
    // https://github.com/anomalyco/opencode/blob/v1.18.29/packages/app/src/context/server.tsx
    format!(
        "#!/bin/sh\n# Written by railway ca desktop --opencode. Re-run setup to update.\nset -eu\nprintf '%s\\n' 'Starting OpenCode. Wait for its listening message, then connect Desktop to:' '  http://localhost.:{port}' 'The trailing dot enables the remote folder picker in OpenCode Desktop.' 'Keep this terminal open. Ctrl-C stops the server and SSH tunnel.'\nexec {ssh}\n"
    )
}

pub(super) fn write_script(path: &Path, script: &str) -> Result<()> {
    crate::util::write_atomic(path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(super) fn remove_script(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupied_local_port_is_reported_before_provisioning() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let error = check_local_port(port).unwrap_err().to_string();
        assert!(error.contains(&port.to_string()));
        assert!(error.contains("--port"));
        // Ask the OS for a free port; the released port could be claimed by
        // another test or process before a second bind.
        check_local_port(0).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn connection_failures_are_returned_to_the_cli() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "exit 23"]);
        let error = run(command).unwrap_err().to_string();
        assert!(error.contains("23"), "{error}");
    }

    #[test]
    fn script_replaces_only_the_same_agents_connection() {
        let home = tempfile::tempdir().unwrap();
        let path = script_path(home.path(), "../../outside");
        assert_eq!(
            path.parent().unwrap(),
            home.path().join(".railway/desktop/opencode")
        );
        let other = script_path(home.path(), "other");
        write_script(&other, "other connection").unwrap();
        write_script(&path, "first").unwrap();
        write_script(&path, "updated").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert!(remove_script(&path).unwrap());
        assert!(!remove_script(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "other connection");
    }

    #[cfg(unix)]
    #[test]
    fn direct_launch_and_script_preserve_arguments_through_both_shells() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("project ' with $(touch INJECTED)");
        std::fs::create_dir(&dir).unwrap();
        // Record the local SSH argv, then emulate OpenSSH's remote shell and
        // bash's -lc boundary. A fake bash avoids loading the tester's profile.
        for (bin, contents) in [
            (
                "ssh",
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$TEST_ARGS\"\nfor arg do remote=$arg; done\nexec /bin/sh -c \"$remote\"\n",
            ),
            ("bash", "#!/bin/sh\nexec /bin/sh -c \"$2\"\n"),
            (
                "opencode",
                "#!/bin/sh\npwd > \"$TEST_CWD\"\nprintf '%s\\n' \"$@\" > \"$TEST_SERVER_ARGS\"\n",
            ),
        ] {
            let path = tmp.path().join(bin);
            std::fs::write(&path, contents).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let script = tmp.path().join("connect.sh");
        let config = tmp.path().join("ssh ' config");
        write_script(
            &script,
            &render_script(
                "railway-agent-box",
                dir.to_str().unwrap(),
                &config,
                15432,
                4097,
            ),
        )
        .unwrap();
        let mut script_command = std::process::Command::new("/bin/sh");
        script_command.arg(&script);
        for mut command in [
            script_command,
            connection_command(
                "railway-agent-box",
                dir.to_str().unwrap(),
                &config,
                15432,
                4097,
            ),
        ] {
            command
                .current_dir(tmp.path())
                .env("PATH", format!("{}:/usr/bin:/bin", tmp.path().display()))
                .env("TEST_ARGS", tmp.path().join("args"))
                .env("TEST_CWD", tmp.path().join("cwd"))
                .env("TEST_SERVER_ARGS", tmp.path().join("server-args"));
            run(command).unwrap();
            let args = std::fs::read_to_string(tmp.path().join("args")).unwrap();
            assert!(
                args.contains(&format!("-F\n{}\n", config.display())),
                "{args}"
            );
            assert!(args.contains("-L\n127.0.0.1:15432:127.0.0.1:4097\n"));
            assert!(args.contains("ExitOnForwardFailure=yes\n"));
            assert_eq!(
                Path::new(
                    std::fs::read_to_string(tmp.path().join("cwd"))
                        .unwrap()
                        .trim()
                )
                .canonicalize()
                .unwrap(),
                dir.canonicalize().unwrap()
            );
            assert_eq!(
                std::fs::read_to_string(tmp.path().join("server-args")).unwrap(),
                "serve\n--hostname\n127.0.0.1\n--port\n4097\n"
            );
            assert!(!tmp.path().join("INJECTED").exists());
            for name in ["args", "cwd", "server-args"] {
                std::fs::remove_file(tmp.path().join(name)).unwrap();
            }
        }
    }
}
