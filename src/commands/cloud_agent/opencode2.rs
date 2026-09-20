//! Seed a separate executable shim; the official V2 runtime is fetched at launch.
use crate::util::shell::shell_join;

pub(crate) mod auth;

pub(super) const SHIM: &str = include_str!("opencode2.py");

pub(crate) fn seed_script() -> String {
    format!(
        "mkdir -p ~/.local/bin ~/.railway/runtimes/opencode2\nprintf '%s' {} > ~/.railway/runtimes/opencode2/launcher.py\nchmod 700 ~/.railway/runtimes/opencode2/launcher.py\nif [ -e ~/.local/bin/opencode2 ] && ! grep -Eq 'railway/runtimes/opencode2|Install the current OpenCode|OpenCode.*Beta' ~/.local/bin/opencode2; then\n  echo 'A custom ~/.local/bin/opencode2 exists; move it before configuring OpenCode.' >&2; exit 1\nfi\nprintf '%s\\n' '#!/bin/sh' 'exec python3 \"$HOME/.railway/runtimes/opencode2/launcher.py\" \"$@\"' > ~/.local/bin/opencode2\nchmod 700 ~/.local/bin/opencode2\n~/.local/bin/opencode2 --railway-import-auth || exit 1",
        shell_join(&[SHIM.to_string()])
    )
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn beta_credentials_merge_without_overwriting_remote_signins() {
        let output = std::process::Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/opencode2_auth.py"
            ))
            .output()
            .expect("python3 is required for OpenCode2 credential tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn beta_installer_verifies_packages_and_preserves_launch_semantics() {
        let output = std::process::Command::new("python3")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/opencode2.py"))
            .output()
            .expect("python3 is required for OpenCode2 installer tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
