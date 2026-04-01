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

        // Resolve symlinks so that e.g. /usr/local/bin/railway (Intel
        // Homebrew symlink) is followed to /usr/local/Cellar/… and
        // correctly classified as Homebrew rather than Shell.
        let exe_path = exe_path.canonicalize().unwrap_or(exe_path);

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

        // Paths owned by system or non-shell package managers — must be
        // checked before the catch-all so we don't misclassify them as Shell.
        const SYSTEM_PATHS: &[&str] = &[
            "/usr/bin",
            "/usr/sbin",
            "/nix/",
            "nix-profile",
            "/snap/",
            "/flatpak/",
        ];
        if SYSTEM_PATHS.iter().any(|p| path_str.contains(p)) {
            return InstallMethod::Unknown;
        }

        // Match well-known shell-installer directories rather than any
        // directory named "bin".  The previous catch-all could misclassify
        // binaries managed by version managers (asdf, mise, proto, etc.)
        // that also live under a `.../bin/` tree, leading to unwanted
        // in-place self-replacement of binaries Railway doesn't own.
        if let Some(home) = dirs::home_dir() {
            let home_str = home.to_string_lossy().to_lowercase();
            let known_shell_dirs: Vec<String> = vec![
                format!("{home_str}/.railway/bin"),
                format!("{home_str}/bin"),
                format!("{home_str}/.local/bin"),
                "/opt/bin".to_string(),
            ];
            if let Some(parent) = exe_path.parent() {
                let parent_str = parent.to_string_lossy().to_lowercase();
                if known_shell_dirs.contains(&parent_str) {
                    return InstallMethod::Shell;
                }
            }
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
    /// Only Shell installs on platforms with published release assets qualify.
    /// Unknown means we don't know where the binary came from, so
    /// self-updating it could conflict with an undetected package manager.
    pub fn can_self_update(&self) -> bool {
        matches!(self, InstallMethod::Shell) && is_self_update_platform()
    }

    /// Whether the current process can write to the directory containing the
    /// binary.  Returns `false` for paths like `/usr/local/bin` that were
    /// installed with `sudo` and are not writable by the current user.
    pub fn can_write_binary(&self) -> bool {
        let exe_path = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let dir = match exe_path.parent() {
            Some(d) => d,
            None => return false,
        };

        // Try creating a temp file in the same directory — the most reliable
        // cross-platform writability check (accounts for ACLs, mount flags…).
        let probe = dir.join(".railway-write-probe");
        let writable = std::fs::File::create(&probe).is_ok();
        let _ = std::fs::remove_file(&probe);
        writable
    }

    /// Whether this install method supports auto-running the package manager
    /// in the background.  Homebrew and Cargo are excluded because they can
    /// take several minutes and would keep a detached process alive far longer
    /// than is acceptable for a transparent background update.
    ///
    /// Also checks that the package manager's global install directory is
    /// writable by the current user, so we don't spawn a doomed `npm update -g`
    /// (installed via `sudo`) that fails immediately on every invocation.
    pub fn can_auto_run_package_manager(&self) -> bool {
        if !matches!(
            self,
            InstallMethod::Npm | InstallMethod::Bun | InstallMethod::Scoop
        ) {
            return false;
        }

        // Probe writability of the directory containing the binary — if we
        // can't write there, the package manager update will fail anyway.
        self.can_write_binary()
    }

    /// Human-readable description of the auto-update strategy for this install method.
    /// Reflects the actual runtime behaviour by checking platform support and
    /// binary writability, so `autoupdate status` never overpromises.
    pub fn update_strategy(&self) -> &'static str {
        match self {
            InstallMethod::Shell if self.can_self_update() && self.can_write_binary() => {
                "Background download + auto-swap"
            }
            InstallMethod::Shell if self.can_self_update() => {
                "Notification only (binary not writable)"
            }
            InstallMethod::Shell => "Notification only (unsupported platform)",
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

/// Returns `true` when the release pipeline publishes a binary for the
/// current OS, i.e. self-update can actually download an asset.
/// FreeBSD is recognized by the install script but no release asset is
/// published, so it must not enter the self-update path.
fn is_self_update_platform() -> bool {
    matches!(std::env::consts::OS, "macos" | "linux" | "windows")
}
