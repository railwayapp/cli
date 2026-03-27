#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Homebrew,
    Npm,
    Bun,
    Cargo,
    Shell,
    Scoop,
    Unknown,
}

impl InstallMethod {
    pub fn detect() -> Self {
        let exe_path = match std::env::current_exe() {
            Ok(path) => path,
            Err(_) => return InstallMethod::Unknown,
        };

        let path_str = exe_path.to_string_lossy().to_lowercase();

        if path_str.contains("homebrew")
            || path_str.contains("cellar")
            || path_str.contains("linuxbrew")
        {
            return InstallMethod::Homebrew;
        }

        // Check for Bun global install (must be before npm since bun uses node_modules internally)
        if path_str.contains(".bun") {
            return InstallMethod::Bun;
        }

        if path_str.contains("node_modules")
            || path_str.contains("npm")
            || path_str.contains(".npm")
        {
            return InstallMethod::Npm;
        }

        if path_str.contains(".cargo") && path_str.contains("bin") {
            return InstallMethod::Cargo;
        }

        if path_str.contains("scoop") {
            return InstallMethod::Scoop;
        }

        if path_str.contains("/usr/local/bin") || path_str.contains("/.local/bin") {
            return InstallMethod::Shell;
        }

        if path_str.contains("program files") || path_str.contains("programfiles") {
            return InstallMethod::Shell;
        }

        InstallMethod::Unknown
    }

    pub fn name(&self) -> &'static str {
        match self {
            InstallMethod::Homebrew => "Homebrew",
            InstallMethod::Npm => "npm",
            InstallMethod::Bun => "Bun",
            InstallMethod::Cargo => "Cargo",
            InstallMethod::Shell => "Shell script",
            InstallMethod::Scoop => "Scoop",
            InstallMethod::Unknown => "Unknown",
        }
    }

    pub fn upgrade_command(&self) -> Option<String> {
        if let Some((program, args)) = self.package_manager_command() {
            return Some(format!("{} {}", program, args.join(" ")));
        }
        match self {
            InstallMethod::Shell => Some("bash <(curl -fsSL cli.new)".to_string()),
            _ => None,
        }
    }

    pub fn can_auto_upgrade(&self) -> bool {
        matches!(
            self,
            InstallMethod::Homebrew
                | InstallMethod::Npm
                | InstallMethod::Bun
                | InstallMethod::Cargo
                | InstallMethod::Scoop
        )
    }

    /// Whether this install method supports direct binary self-update
    /// (download from GitHub Releases and replace in place).
    /// Only Shell installs qualify — Unknown means we don't know where the
    /// binary came from, so self-updating it could conflict with an
    /// undetected package manager.
    pub fn can_self_update(&self) -> bool {
        matches!(self, InstallMethod::Shell)
    }

    /// Whether this install method supports auto-running the package manager
    /// in the background (fast package managers only).
    pub fn can_auto_run_package_manager(&self) -> bool {
        matches!(
            self,
            InstallMethod::Npm | InstallMethod::Bun | InstallMethod::Scoop
        )
    }

    /// Human-readable description of the auto-update strategy for this install method.
    pub fn update_strategy(&self) -> &'static str {
        match self {
            InstallMethod::Shell => "Background download + auto-swap",
            InstallMethod::Npm | InstallMethod::Bun | InstallMethod::Scoop => {
                "Auto-run package manager"
            }
            InstallMethod::Homebrew | InstallMethod::Cargo | InstallMethod::Unknown => {
                "Notification only (manual upgrade)"
            }
        }
    }

    /// Returns the program and arguments to run the package manager upgrade.
    pub fn package_manager_command(&self) -> Option<(&'static str, Vec<&'static str>)> {
        match self {
            InstallMethod::Homebrew => Some(("brew", vec!["upgrade", "railway"])),
            InstallMethod::Npm => Some(("npm", vec!["update", "-g", "@railway/cli"])),
            InstallMethod::Bun => Some(("bun", vec!["update", "-g", "@railway/cli"])),
            InstallMethod::Cargo => Some(("cargo", vec!["install", "railwayapp"])),
            InstallMethod::Scoop => Some(("scoop", vec!["update", "railway"])),
            InstallMethod::Shell | InstallMethod::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_a_variant() {
        // Just verify it doesn't panic on the current platform
        let _ = InstallMethod::detect();
    }

    #[test]
    fn upgrade_command_derived_from_package_manager_command() {
        // For methods with a package manager command, upgrade_command should
        // produce the same string as joining program + args.
        for method in [
            InstallMethod::Homebrew,
            InstallMethod::Npm,
            InstallMethod::Bun,
            InstallMethod::Cargo,
            InstallMethod::Scoop,
        ] {
            let (program, args) = method.package_manager_command().unwrap();
            let expected = format!("{} {}", program, args.join(" "));
            assert_eq!(method.upgrade_command().unwrap(), expected);
        }
    }

    #[test]
    fn upgrade_command_shell_is_curl_script() {
        let cmd = InstallMethod::Shell.upgrade_command().unwrap();
        assert!(cmd.contains("curl"));
    }

    #[test]
    fn upgrade_command_unknown_is_none() {
        assert!(InstallMethod::Unknown.upgrade_command().is_none());
    }

    #[test]
    fn self_update_only_for_shell() {
        assert!(InstallMethod::Shell.can_self_update());
        assert!(!InstallMethod::Homebrew.can_self_update());
        assert!(!InstallMethod::Npm.can_self_update());
        assert!(!InstallMethod::Unknown.can_self_update());
    }

    #[test]
    fn auto_run_excludes_slow_managers() {
        assert!(!InstallMethod::Homebrew.can_auto_run_package_manager());
        assert!(!InstallMethod::Cargo.can_auto_run_package_manager());
        assert!(InstallMethod::Npm.can_auto_run_package_manager());
        assert!(InstallMethod::Bun.can_auto_run_package_manager());
        assert!(InstallMethod::Scoop.can_auto_run_package_manager());
    }

    #[test]
    fn every_variant_has_a_name() {
        for method in [
            InstallMethod::Homebrew,
            InstallMethod::Npm,
            InstallMethod::Bun,
            InstallMethod::Cargo,
            InstallMethod::Shell,
            InstallMethod::Scoop,
            InstallMethod::Unknown,
        ] {
            assert!(!method.name().is_empty());
        }
    }

    #[test]
    fn every_variant_has_an_update_strategy() {
        for method in [
            InstallMethod::Homebrew,
            InstallMethod::Npm,
            InstallMethod::Bun,
            InstallMethod::Cargo,
            InstallMethod::Shell,
            InstallMethod::Scoop,
            InstallMethod::Unknown,
        ] {
            assert!(!method.update_strategy().is_empty());
        }
    }
}
