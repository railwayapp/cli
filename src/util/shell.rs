/// POSIX shell quoting for command strings sent to a remote `sh -c`.
///
/// Plain words pass through untouched so quoted commands stay readable;
/// anything else is single-quoted with embedded quotes escaped via the
/// standard `'\''` dance.
pub fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '='))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Join argv into a single shell command, quoting each argument so the
/// remote shell sees exactly the args the user passed. `exec -- bash -lc
/// 'echo a | b'` must arrive as `bash -lc 'echo a | b'`, not re-split on
/// the pipe.
pub fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_pass_through() {
        assert_eq!(shell_quote("ls"), "ls");
        assert_eq!(shell_quote("/usr/bin/env"), "/usr/bin/env");
        assert_eq!(shell_quote("FOO=bar"), "FOO=bar");
    }

    #[test]
    fn metacharacters_are_quoted() {
        assert_eq!(shell_quote("a | b"), "'a | b'");
        assert_eq!(shell_quote("a && b"), "'a && b'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn single_quotes_are_escaped() {
        assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
    }

    #[test]
    fn join_preserves_arg_boundaries() {
        let args = vec![
            "bash".to_string(),
            "-lc".to_string(),
            "echo hi | base64 -d > /tmp/f && cat /tmp/f".to_string(),
        ];
        assert_eq!(
            shell_join(&args),
            "bash -lc 'echo hi | base64 -d > /tmp/f && cat /tmp/f'"
        );
    }
}
