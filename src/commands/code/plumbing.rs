//! Send setup scripts over SSH stdin, followed by their credential/skills input.
use crate::util::shell::shell_join;

// OpenSSH sends the command through its local ControlMaster socket before
// passing stdin/stdout/stderr descriptors. Large commands can fill that socket
// on macOS and fail with `mm_send_fd: sendmsg(0): Message too long`. Keep the
// command small while preserving connection reuse and the original stdin.
// Python is part of the cloud-agent base image. Unbuffered, exact-length reads
// are essential: a buffered reader could consume the following credentials.
const READ_SCRIPT: &str = r#"import os, sys
remaining = int(sys.argv[1])
chunks = []
while remaining:
    chunk = os.read(0, min(remaining, 65536))
    if not chunk:
        sys.exit("Incomplete cloud agent setup script")
    chunks.append(chunk)
    remaining -= len(chunk)
os.execvp("sh", ["sh", "-c", b"".join(chunks).decode("utf-8")])
"#;

pub(super) fn script_input(script: &str, input: Option<&[u8]>) -> (String, Vec<u8>) {
    let command = shell_join(&[
        "python3".into(),
        "-c".into(),
        READ_SCRIPT.into(),
        script.len().to_string(),
    ]);
    let mut payload = Vec::with_capacity(script.len() + input.map_or(0, <[u8]>::len));
    payload.extend_from_slice(script.as_bytes());
    payload.extend_from_slice(input.unwrap_or_default());
    (command, payload)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Output, Stdio};

    fn run(command: &str, payload: Vec<u8>) -> Output {
        let mut child = Command::new("sh")
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || {
            // Exercise partial reads, including splits inside UTF-8 characters.
            for chunk in payload.chunks(113) {
                stdin.write_all(chunk).unwrap();
            }
        });
        let output = child.wait_with_output().unwrap();
        writer.join().unwrap();
        output
    }

    #[test]
    fn large_script_preserves_binary_input_and_exit_status() {
        let script = format!(
            "# {}\nprintf 'ready\\n'; cat; printf 'done' >&2; exit 42",
            "🛤'\"$`".repeat(9000)
        );
        let input: Vec<u8> = (0..=255).cycle().take(100_000).collect();
        let (command, payload) = script_input(&script, Some(&input));
        assert!(command.len() < 1024);
        let output = run(&command, payload);
        assert_eq!(output.status.code(), Some(42));
        assert_eq!(output.stdout, [b"ready\n".as_slice(), &input].concat());
        assert_eq!(output.stderr, b"done");
    }

    #[test]
    fn no_payload_reaches_eof() {
        let (command, payload) = script_input("cat; printf ready", None);
        let output = run(&command, payload);
        assert!(output.status.success());
        assert_eq!(output.stdout, b"ready");
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn truncated_script_is_never_executed() {
        let (command, mut payload) = script_input("printf should-not-run\n", None);
        payload.pop();
        let output = run(&command, payload);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"Incomplete cloud agent setup script\n");
    }

    #[test]
    fn beta_provisioning_keeps_the_ssh_command_small() {
        let script = super::super::provision_script_with_skills(
            super::super::Agent::OpenCode2,
            Some(42),
            true,
            "deadbeef",
        );
        let (command, payload) = script_input(&script, Some(&[0; 64]));
        assert!(command.len() < 1024);
        assert_eq!(&payload[..script.len()], script.as_bytes());
        assert_eq!(&payload[script.len()..], &[0; 64]);
    }
}
