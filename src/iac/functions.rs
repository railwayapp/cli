//! Railway Functions: single-file Bun services whose source is base64-encoded
//! into the deploy start command (`./run.sh <base64>`). The platform models
//! them as image services on the function runtime image; the SDKs expose them
//! as `fn(name, { code })`.

use base64::prelude::*;
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

use super::json::field_str;

/// Every function runtime image lives under this prefix; mirrors
/// `is_function_service` in `resources.rs` and the dashboard's check.
pub const FUNCTION_IMAGE_PREFIX: &str = "ghcr.io/railwayapp/function";

const START_COMMAND_PREFIX: &str = "./run.sh ";

pub fn is_function_image(image: &str) -> bool {
    image.starts_with(FUNCTION_IMAGE_PREFIX)
}

/// True for a graph node that is a function: declared with `fn()` (kind
/// `function`), or a service running the function runtime image.
pub fn is_function_resource(resource: &Value) -> bool {
    if field_str(resource, "type") != Some("service") {
        return false;
    }
    if field_str(resource, "kind") == Some("function") {
        return true;
    }
    resource
        .get("source")
        .and_then(|source| field_str(source, "image"))
        .is_some_and(is_function_image)
}

/// Decodes the source code carried by a function start command. `None` when
/// the command is not in the `./run.sh <base64>` form or the payload is not
/// UTF-8, in which case callers fall back to the raw string.
pub fn decode_function_code(start_command: &str) -> Option<String> {
    let encoded = start_command.strip_prefix(START_COMMAND_PREFIX)?.trim();
    if encoded.is_empty() || encoded.contains(' ') {
        return None;
    }
    let bytes = BASE64_STANDARD.decode(encoded).ok()?;
    String::from_utf8(bytes).ok()
}

/// Encodes source code the way `railway functions push` and the SDKs do.
#[cfg(test)]
pub fn encode_function_code(code: &str) -> String {
    format!("{START_COMMAND_PREFIX}{}", BASE64_STANDARD.encode(code))
}

/// The decoded source of a service's `deploy.startCommand`, when it is a
/// function start command.
pub fn function_code_of(deploy: &Value) -> Option<String> {
    field_str(deploy, "startCommand").and_then(decode_function_code)
}

/// Unified diff lines (no file header) between two sources, plus the
/// insertion and deletion counts. Empty when the sources are identical.
pub fn source_code_diff_lines(before: &str, after: &str) -> (Vec<String>, usize, usize) {
    let diff = TextDiff::from_lines(before, after);
    let mut lines = Vec::new();
    let mut insertions = 0;
    let mut deletions = 0;
    for (index, group) in diff.grouped_ops(3).iter().enumerate() {
        if index > 0 {
            lines.push("...".to_string());
        }
        for op in group {
            for change in diff.iter_changes(op) {
                let marker = match change.tag() {
                    ChangeTag::Delete => {
                        deletions += 1;
                        "-"
                    }
                    ChangeTag::Insert => {
                        insertions += 1;
                        "+"
                    }
                    ChangeTag::Equal => " ",
                };
                let text = change.value().trim_end_matches(['\n', '\r']);
                lines.push(format!("{marker} {text}"));
            }
        }
    }
    (lines, insertions, deletions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_source_through_the_start_command() {
        let code = "export default { fetch() { return new Response(\"hi\"); } }\n";
        assert_eq!(
            decode_function_code(&encode_function_code(code)).as_deref(),
            Some(code)
        );
    }

    #[test]
    fn rejects_commands_that_are_not_function_start_commands() {
        assert_eq!(decode_function_code("npm start"), None);
        assert_eq!(decode_function_code("./run.sh"), None);
        assert_eq!(decode_function_code("./run.sh not base64!"), None);
        assert_eq!(decode_function_code("./run.sh aGk= extra"), None);
    }

    #[test]
    fn classifies_function_resources_by_kind_or_image() {
        assert!(is_function_resource(
            &json!({ "type": "service", "kind": "function" })
        ));
        assert!(is_function_resource(&json!({
            "type": "service",
            "kind": "docker-image",
            "source": { "type": "image", "image": "ghcr.io/railwayapp/function:1.2.3" }
        })));
        assert!(!is_function_resource(&json!({
            "type": "service",
            "kind": "docker-image",
            "source": { "type": "image", "image": "ghcr.io/acme/api:1" }
        })));
        assert!(!is_function_resource(
            &json!({ "type": "database", "kind": "function" })
        ));
    }

    #[test]
    fn diff_lines_carry_markers_and_counts() {
        let (lines, insertions, deletions) = source_code_diff_lines("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(lines, vec!["  a", "- b", "+ B", "  c"]);
        assert_eq!((insertions, deletions), (1, 1));
        assert!(source_code_diff_lines("same\n", "same\n").0.is_empty());
    }
}
