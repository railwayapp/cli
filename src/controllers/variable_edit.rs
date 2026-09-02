//! Bulk variable editing via `$EDITOR`, with an IaC-style diff confirm before apply.
//!
//! Flow: fetch unrendered user vars → write dotenv temp file → open editor → parse → diff → confirm → upsert/delete.

use crate::controllers::variables::{
    EditSnapshot, EditVariableEntry, SEALED_TOKEN, is_railway_reserved_key,
};
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const REDACTED: &str = "«hidden»";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarChangeKind {
    Set,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarChange {
    pub kind: VarChangeKind,
    pub key: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl VarChange {
    pub fn is_destructive(&self) -> bool {
        matches!(self.kind, VarChangeKind::Delete)
    }

    pub fn summary(&self, service: &str) -> String {
        match self.kind {
            VarChangeKind::Set => format!("Set variable {service}.{}", self.key),
            VarChangeKind::Update => format!("Update variable {service}.{}", self.key),
            VarChangeKind::Delete => format!("Delete variable {service}.{}", self.key),
        }
    }

    pub fn detail(&self, service: &str, reveal: bool) -> String {
        let before = format_value(self.before.as_deref(), reveal);
        let after = format_value(self.after.as_deref(), reveal);
        match self.kind {
            VarChangeKind::Delete => format!("{service}.{} ({before} → ∅)", self.key),
            _ => format!("{service}.{} ({before} → {after})", self.key),
        }
    }
}

fn format_value(value: Option<&str>, reveal: bool) -> String {
    match value {
        None => "∅".into(),
        Some(SEALED_TOKEN) => SEALED_TOKEN.into(),
        Some("") => "\"\"".into(),
        Some(v) if reveal => truncate_for_display(v),
        Some(_) => REDACTED.into(),
    }
}

fn truncate_for_display(value: &str) -> String {
    const MAX: usize = 64;
    let flat = value.replace('\n', "\\n");
    if flat.chars().count() <= MAX {
        return flat;
    }
    let truncated: String = flat.chars().take(MAX).collect();
    format!("{truncated}…")
}

/// Diff editable variables from `before` → parsed `after`.
pub fn diff_edit_snapshot(
    before: &EditSnapshot,
    after: &BTreeMap<String, EditVariableEntry>,
) -> Vec<VarChange> {
    let before_values = editable_values(&before.editable);
    let after_values = editable_values(after);
    diff_variables(&before_values, &after_values)
}

/// Diff `before` → `after`. Keys only in `after` are sets; only in `before` are deletes;
/// present in both with different values are updates. Order is stable (BTreeMap).
pub fn diff_variables(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<VarChange> {
    let mut changes = Vec::new();

    for (key, after_value) in after {
        if is_railway_reserved_key(key) {
            continue;
        }
        match before.get(key) {
            None => changes.push(VarChange {
                kind: VarChangeKind::Set,
                key: key.clone(),
                before: None,
                after: Some(after_value.clone()),
            }),
            Some(before_value) if before_value != after_value => changes.push(VarChange {
                kind: VarChangeKind::Update,
                key: key.clone(),
                before: Some(before_value.clone()),
                after: Some(after_value.clone()),
            }),
            Some(_) => {}
        }
    }

    for (key, before_value) in before {
        if is_railway_reserved_key(key) {
            continue;
        }
        if !after.contains_key(key) {
            changes.push(VarChange {
                kind: VarChangeKind::Delete,
                key: key.clone(),
                before: Some(before_value.clone()),
                after: None,
            });
        }
    }

    changes
}

fn editable_values(editable: &BTreeMap<String, EditVariableEntry>) -> BTreeMap<String, String> {
    editable
        .iter()
        .map(|(key, entry)| (key.clone(), entry.value.clone()))
        .collect()
}

pub fn plan_summary_line(changes: &[VarChange]) -> String {
    let mut add = 0;
    let mut change = 0;
    let mut destroy = 0;
    for c in changes {
        match c.kind {
            VarChangeKind::Set => add += 1,
            VarChangeKind::Update => change += 1,
            VarChangeKind::Delete => destroy += 1,
        }
    }
    format!(
        "{} {}, {}, {}",
        "Plan:".bold(),
        format!("{add} to add").green(),
        format!("{change} to change").yellow(),
        format!("{destroy} to destroy").red(),
    )
}

/// Render an IaC-flavoured variable plan to stdout.
pub fn print_variable_plan(service: &str, changes: &[VarChange], reveal: bool) {
    println!();
    println!("{}", "Railway variables".bold());
    println!("{} {}", "Service".dimmed(), service.cyan());
    println!();

    if changes.is_empty() {
        println!("{}", "✓ No variable changes.".green());
        return;
    }

    println!("{}", plan_summary_line(changes));
    for change in changes {
        let marker = match change.kind {
            VarChangeKind::Set => "+".green().bold(),
            VarChangeKind::Update => "~".yellow().bold(),
            VarChangeKind::Delete => "-".red().bold(),
        };
        println!("  {} {}", marker, change.summary(service));
        println!(
            "    {} {}",
            "└".dimmed(),
            change.detail(service, reveal).dimmed()
        );
    }

    let destructive = changes.iter().filter(|c| c.is_destructive()).count();
    if destructive > 0 {
        println!();
        println!(
            "{} {}",
            "!".red().bold(),
            format!("{destructive} destructive change(s) will remove variables.").red()
        );
    }
}

pub fn render_variable_plan_plain(service: &str, changes: &[VarChange], reveal: bool) -> String {
    if changes.is_empty() {
        return "No changes.".into();
    }
    changes
        .iter()
        .map(|change| {
            let marker = match change.kind {
                VarChangeKind::Set => "+",
                VarChangeKind::Update => "~",
                VarChangeKind::Delete => "-",
            };
            format!(
                "{marker} {}\n    {}",
                change.summary(service),
                change.detail(service, reveal)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Serialize an edit snapshot as a dotenv document with a Railway header.
pub fn write_edit_document(
    path: &Path,
    snapshot: &EditSnapshot,
    header_lines: &[&str],
) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("Failed to create temp file {}", path.display()))?;

    for line in header_lines {
        writeln!(file, "# {line}")?;
    }
    if !header_lines.is_empty() {
        writeln!(file)?;
    }

    for (key, value) in &snapshot.read_only {
        writeln!(file, "# {key}={}", escape_dotenv_value(value))?;
    }
    if !snapshot.read_only.is_empty() {
        writeln!(file, "# (Railway-provided variables above are read-only)")?;
        writeln!(file)?;
    }

    for (key, entry) in &snapshot.editable {
        writeln!(file, "{}={}", key, escape_dotenv_value(&entry.value))?;
    }

    file.flush()?;
    Ok(())
}

fn escape_dotenv_value(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value.contains([' ', '\t', '\n', '\r', '#', '"', '\'', '\\'])
        || value.starts_with(['=', '#']);

    if !needs_quotes {
        return value.to_string();
    }

    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!("\"{escaped}\"")
}

/// Parse editable variables from a dotenv document produced/edited by `variable edit`.
///
/// Comment lines (including read-only Railway-provided variables) are ignored.
pub fn parse_edit_document(contents: &str) -> Result<BTreeMap<String, EditVariableEntry>> {
    let mut vars = BTreeMap::new();

    for (idx, raw_line) in contents.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            bail!("Invalid dotenv on line {line_no}: expected KEY=VALUE, got {raw_line:?}");
        };

        let key = key.trim();
        if key.is_empty() || key.chars().any(|c| c.is_whitespace()) {
            bail!("Invalid variable name on line {line_no}: {key:?}");
        }
        if is_railway_reserved_key(key) {
            bail!(
                "Line {line_no}: `{key}` is a Railway-provided variable and cannot be edited here. Remove it from the editable section."
            );
        }

        let value = unquote_dotenv_value(value.trim())
            .with_context(|| format!("Invalid quoted value on line {line_no}"))?;

        if vars
            .insert(
                key.to_string(),
                EditVariableEntry {
                    value,
                    is_sealed: false,
                },
            )
            .is_some()
        {
            bail!("Duplicate variable {key:?} on line {line_no}");
        }
    }

    Ok(vars)
}

fn unquote_dotenv_value(value: &str) -> Result<String> {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            let inner = &value[1..value.len() - 1];
            if bytes[0] == b'\'' {
                return Ok(inner.to_string());
            }
            return Ok(unescape_double_quoted(inner));
        }
    }
    Ok(value.to_string())
}

fn unescape_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn resolve_editor() -> Result<String> {
    for key in ["VISUAL", "EDITOR"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
    }

    for fallback in ["nano", "vim", "vi"] {
        if which_exists(fallback) {
            return Ok(fallback.to_string());
        }
    }

    bail!("No editor found. Set $VISUAL or $EDITOR, e.g.:\n  EDITOR=vim railway variable edit")
}

fn which_exists(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(bin);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

/// Open `path` in `$VISUAL` / `$EDITOR`. Non-zero exit aborts the edit.
pub fn open_in_editor(path: &Path) -> Result<()> {
    let editor = resolve_editor()?;
    let parts = shlex::split(&editor)
        .with_context(|| format!("Failed to parse editor command: {editor:?}"))?;
    if parts.is_empty() {
        bail!("Editor command is empty");
    }

    let status = Command::new(&parts[0])
        .args(&parts[1..])
        .arg(path)
        .status()
        .with_context(|| format!("Failed to launch editor `{editor}`"))?;

    if !status.success() {
        bail!("Editor exited with {status} — no changes applied");
    }
    Ok(())
}

pub fn temp_edit_path(service: &str) -> PathBuf {
    let safe: String = service
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    std::env::temp_dir().join(format!(
        "railway-vars-{}-{}-{}.env",
        safe,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ))
}

/// Build apply-ready upserts/deletes from a diff.
///
/// A sealed variable the user left alone never reaches here: its before and after
/// are both [`SEALED_TOKEN`], so the diff reports no change and the stored ciphertext
/// is untouched. That makes `<sealed>` in an `after` value unambiguous — the user typed
/// it by hand on a variable that is not sealed, which we cannot honour either way.
pub fn applyable_changes(
    before: &EditSnapshot,
    changes: &[VarChange],
) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut upserts = BTreeMap::new();
    let mut deletes = Vec::new();

    for change in changes {
        match change.kind {
            VarChangeKind::Set | VarChangeKind::Update => {
                let after = change.after.clone().unwrap_or_default();
                if after == SEALED_TOKEN {
                    let sealed_before = before
                        .editable
                        .get(&change.key)
                        .is_some_and(|entry| entry.is_sealed);
                    if sealed_before {
                        bail!(
                            "`{}` is sealed and its value cannot be read back. Leave the line as `{SEALED_TOKEN}` to keep it, or set a new value to rotate it.",
                            change.key
                        );
                    }
                    bail!(
                        "`{}` is not a sealed variable, so `{SEALED_TOKEN}` is not a value it can take. Set a real value or delete the line.",
                        change.key
                    );
                }
                upserts.insert(change.key.clone(), after);
            }
            VarChangeKind::Delete => deletes.push(change.key.clone()),
        }
    }

    Ok((upserts, deletes))
}

/// Fixture used by `railway variable edit --demo`.
pub fn demo_snapshot() -> EditSnapshot {
    EditSnapshot {
        editable: BTreeMap::from([
            (
                "DATABASE_URL".into(),
                EditVariableEntry {
                    value: "postgresql://postgres:password@postgres.railway.internal:5432/railway"
                        .into(),
                    is_sealed: false,
                },
            ),
            (
                "FEATURE_OLD".into(),
                EditVariableEntry {
                    value: "1".into(),
                    is_sealed: false,
                },
            ),
            (
                "LOG_LEVEL".into(),
                EditVariableEntry {
                    value: "info".into(),
                    is_sealed: false,
                },
            ),
            (
                "REDIS_URL".into(),
                EditVariableEntry {
                    value: "redis://redis.railway.internal:6379".into(),
                    is_sealed: false,
                },
            ),
            (
                "STRIPE_SECRET_KEY".into(),
                EditVariableEntry {
                    value: SEALED_TOKEN.into(),
                    is_sealed: true,
                },
            ),
        ]),
        read_only: BTreeMap::from([
            ("RAILWAY_ENVIRONMENT_NAME".into(), "production".into()),
            (
                "RAILWAY_PRIVATE_DOMAIN".into(),
                "api.railway.internal".into(),
            ),
            ("RAILWAY_PROJECT_NAME".into(), "demo".into()),
            ("RAILWAY_SERVICE_NAME".into(), "api".into()),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_set_update_delete() {
        let before = BTreeMap::from([
            ("A".into(), "1".into()),
            ("B".into(), "2".into()),
            ("C".into(), "3".into()),
        ]);
        let after = BTreeMap::from([
            ("A".into(), "1".into()),
            ("B".into(), "changed".into()),
            ("D".into(), "new".into()),
        ]);
        let changes = diff_variables(&before, &after);
        assert_eq!(
            changes
                .iter()
                .map(|c| (c.kind.clone(), c.key.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (VarChangeKind::Update, "B"),
                (VarChangeKind::Set, "D"),
                (VarChangeKind::Delete, "C"),
            ]
        );
    }

    #[test]
    fn dotenv_round_trip_quotes_specials() {
        let snapshot = EditSnapshot {
            editable: BTreeMap::from([
                (
                    "PLAIN".into(),
                    EditVariableEntry {
                        value: "ok".into(),
                        is_sealed: false,
                    },
                ),
                (
                    "EMPTY".into(),
                    EditVariableEntry {
                        value: "".into(),
                        is_sealed: false,
                    },
                ),
                (
                    "SPACED".into(),
                    EditVariableEntry {
                        value: "hello world".into(),
                        is_sealed: false,
                    },
                ),
                (
                    "HASH".into(),
                    EditVariableEntry {
                        value: "a#b".into(),
                        is_sealed: false,
                    },
                ),
                (
                    "NL".into(),
                    EditVariableEntry {
                        value: "line1\nline2".into(),
                        is_sealed: false,
                    },
                ),
            ]),
            read_only: BTreeMap::new(),
        };
        let path = temp_edit_path("test");
        write_edit_document(&path, &snapshot, &["header"]).unwrap();
        let parsed = parse_edit_document(&fs::read_to_string(&path).unwrap()).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(
            parsed
                .into_iter()
                .map(|(k, v)| (k, v.value))
                .collect::<BTreeMap<_, _>>(),
            editable_values(&snapshot.editable)
        );
    }

    #[test]
    fn read_only_railway_vars_are_comments_only() {
        let snapshot = demo_snapshot();
        let path = temp_edit_path("demo");
        write_edit_document(&path, &snapshot, &["demo"]).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert!(contents.contains("# RAILWAY_SERVICE_NAME=api"));
        let parsed = parse_edit_document(&contents).unwrap();
        assert!(!parsed.contains_key("RAILWAY_SERVICE_NAME"));
        assert_eq!(parsed.get("STRIPE_SECRET_KEY").unwrap().value, SEALED_TOKEN);
    }

    #[test]
    fn rejects_railway_keys_in_editable_section() {
        let err = parse_edit_document("RAILWAY_FOO=bar\n").unwrap_err();
        assert!(err.to_string().contains("Railway-provided"));
    }

    #[test]
    fn sealed_preserve_skips_upsert() {
        let before = demo_snapshot();
        let after = before.clone();
        let changes = diff_edit_snapshot(&before, &after.editable);
        assert!(changes.is_empty());
        let (upserts, deletes) = applyable_changes(&before, &changes).unwrap();
        assert!(upserts.is_empty());
        assert!(deletes.is_empty());
    }

    #[test]
    fn sealed_rotate_emits_update() {
        let before = demo_snapshot();
        let mut after = before.editable.clone();
        after.insert(
            "STRIPE_SECRET_KEY".into(),
            EditVariableEntry {
                value: "sk_live_new".into(),
                is_sealed: true,
            },
        );
        let changes = diff_edit_snapshot(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, VarChangeKind::Update);
        let (upserts, _) = applyable_changes(&before, &changes).unwrap();
        assert_eq!(
            upserts.get("STRIPE_SECRET_KEY").map(String::as_str),
            Some("sk_live_new")
        );
    }

    #[test]
    fn hand_typed_sealed_token_on_plain_variable_is_rejected() {
        let before = demo_snapshot();
        let mut after = before.editable.clone();
        after.insert(
            "LOG_LEVEL".into(),
            EditVariableEntry {
                value: SEALED_TOKEN.into(),
                is_sealed: false,
            },
        );
        let changes = diff_edit_snapshot(&before, &after);
        let err = applyable_changes(&before, &changes).unwrap_err();
        assert!(err.to_string().contains("not a sealed variable"));
    }

    #[test]
    fn diff_ignores_railway_reserved_keys() {
        let before = BTreeMap::from([("RAILWAY_FOO".into(), "a".into())]);
        let after = BTreeMap::from([("RAILWAY_FOO".into(), "b".into())]);
        assert!(diff_variables(&before, &after).is_empty());
    }

    #[test]
    fn redacts_by_default_in_plain_render() {
        let changes = vec![VarChange {
            kind: VarChangeKind::Update,
            key: "SECRET".into(),
            before: Some("sk-before".into()),
            after: Some("sk-after".into()),
        }];
        let rendered = render_variable_plan_plain("api", &changes, false);
        assert!(rendered.contains(REDACTED));
        assert!(!rendered.contains("sk-before"));
        assert!(!rendered.contains("sk-after"));
        assert!(rendered.contains("~ Update variable api.SECRET"));
    }

    #[test]
    fn rejects_duplicate_keys() {
        let err = parse_edit_document("A=1\nA=2\n").unwrap_err();
        assert!(err.to_string().contains("Duplicate"));
    }

    #[cfg(unix)]
    #[test]
    fn write_edit_document_is_owner_readable_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_edit_path("perms");
        write_edit_document(&path, &demo_snapshot(), &["header"]).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = fs::remove_file(&path);
        assert_eq!(mode, 0o600);
    }
}
