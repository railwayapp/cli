//! Seed a separate executable shim; the beta runtime is fetched at launch.
use crate::util::shell::shell_join;

pub(super) const SHIM: &str = include_str!("opencode2.py");

pub(crate) fn seed_script() -> String {
    format!(
        "mkdir -p ~/.local/bin\nprintf '%s' {} > ~/.local/bin/opencode2\nchmod 700 ~/.local/bin/opencode2",
        shell_join(&[SHIM.to_string()])
    )
}

#[cfg(all(test, unix))]
mod tests {
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
