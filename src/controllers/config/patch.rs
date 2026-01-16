use anyhow::{Context, Result, bail};
use json_dotpath::DotPaths;
use schemars::{
    schema::{InstanceType, RootSchema, Schema, SingleOrVec},
    schema_for,
};
use similar::get_close_matches;

use super::environment::{EnvironmentConfig, ServiceInstance};

/// A patch entry representing a dot-path and value to set in the EnvironmentConfig
pub type PatchEntry = (String, serde_json::Value);

/// The expected type for a schema field
#[derive(Debug, Clone, PartialEq)]
pub enum ExpectedType {
    String,
    Integer,
    Number,
    Boolean,
    /// Array with inner element type
    Array(Box<ExpectedType>),
    Object,
    /// Nullable version of another type (e.g., Option<String>)
    Nullable(Box<ExpectedType>),
    /// Any type is accepted
    Any,
}

impl std::fmt::Display for ExpectedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpectedType::String => write!(f, "string"),
            ExpectedType::Integer => write!(f, "integer"),
            ExpectedType::Number => write!(f, "number"),
            ExpectedType::Boolean => write!(f, "boolean"),
            ExpectedType::Array(inner) => write!(f, "array of {}", inner),
            ExpectedType::Object => write!(f, "object"),
            ExpectedType::Nullable(inner) => write!(f, "{} (nullable)", inner),
            ExpectedType::Any => write!(f, "any"),
        }
    }
}

/// Validates a path and parses a string value according to the schema's expected type.
/// The path should NOT include the "services.<id>." prefix.
///
/// Normalizes the path by:
/// - Stripping leading/trailing dots
/// - Collapsing consecutive dots
///
/// Returns the parsed JSON value, or an error if the path is invalid or value doesn't match type.
pub fn parse_service_value(path: &str, value: &str) -> Result<serde_json::Value> {
    let normalized = normalize_path(path);
    let root_schema = schema_for!(ServiceInstance);
    let root = Schema::Object(root_schema.schema.clone());

    let segments: Vec<&str> = normalized.split('.').collect();
    let expected_type = get_expected_type(&root_schema, &root, &segments, &normalized)?;

    parse_value_as_type(value, &expected_type, &normalized)
}

/// Normalize a dot-path by stripping leading/trailing dots and collapsing consecutive dots
fn normalize_path(path: &str) -> String {
    path.trim_matches('.')
        .split('.')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

/// Validates that a dot-path is valid for a ServiceInstance schema.
/// The path should NOT include the "services.<id>." prefix.
///
/// Returns Ok(()) if the path is valid, or an error with suggestions if invalid.
#[cfg(test)]
fn validate_service_path(path: &str) -> Result<()> {
    let root_schema = schema_for!(ServiceInstance);
    let root = Schema::Object(root_schema.schema.clone());

    let segments: Vec<&str> = path.split('.').collect();
    get_expected_type(&root_schema, &root, &segments, path)?;
    Ok(())
}

/// Parse a string value according to the expected type
fn parse_value_as_type(
    value: &str,
    expected: &ExpectedType,
    path: &str,
) -> Result<serde_json::Value> {
    match expected {
        ExpectedType::String => Ok(serde_json::json!(value)),

        ExpectedType::Integer => value
            .parse::<i64>()
            .map(|n| serde_json::json!(n))
            .map_err(|_| {
                anyhow::anyhow!(
                    "Invalid value for '{}': expected integer, got '{}'",
                    path,
                    value
                )
            }),

        ExpectedType::Number => {
            // Try integer first, then float
            if let Ok(n) = value.parse::<i64>() {
                Ok(serde_json::json!(n))
            } else {
                value
                    .parse::<f64>()
                    .map(|n| serde_json::json!(n))
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "Invalid value for '{}': expected number, got '{}'",
                            path,
                            value
                        )
                    })
            }
        }

        ExpectedType::Boolean => match value.to_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(serde_json::json!(true)),
            "false" | "0" | "no" => Ok(serde_json::json!(false)),
            _ => bail!(
                "Invalid value for '{}': expected boolean (true/false), got '{}'",
                path,
                value
            ),
        },

        ExpectedType::Array(inner_type) => {
            // If it looks like JSON array, parse as JSON
            if value.trim_start().starts_with('[') {
                let parsed: serde_json::Value = serde_json::from_str(value).map_err(|e| {
                    anyhow::anyhow!(
                        "Invalid value for '{}': expected JSON array, got '{}' ({})",
                        path,
                        value,
                        e
                    )
                })?;
                if !parsed.is_array() {
                    bail!(
                        "Invalid value for '{}': expected array, got '{}'",
                        path,
                        value
                    );
                }
                Ok(parsed)
            } else {
                // Otherwise, comma-split and parse each element as the inner type
                let elements: Result<Vec<serde_json::Value>> = value
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| parse_value_as_type(s, inner_type, path))
                    .collect();
                Ok(serde_json::Value::Array(elements?))
            }
        }

        ExpectedType::Object => {
            // Try to parse as JSON object
            serde_json::from_str::<serde_json::Value>(value)
                .ok()
                .filter(|v| v.is_object())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Invalid value for '{}': expected JSON object, got '{}'",
                        path,
                        value
                    )
                })
        }

        ExpectedType::Nullable(inner) => {
            // Allow "null" as a value
            if value.to_lowercase() == "null" {
                Ok(serde_json::Value::Null)
            } else {
                parse_value_as_type(value, inner, path)
            }
        }

        ExpectedType::Any => {
            // Try JSON first, then fall back to string
            Ok(serde_json::from_str(value).unwrap_or_else(|_| serde_json::json!(value)))
        }
    }
}

/// Get the expected type for a path by traversing the schema
fn get_expected_type(
    root_schema: &RootSchema,
    schema: &Schema,
    segments: &[&str],
    full_path: &str,
) -> Result<ExpectedType> {
    match schema {
        Schema::Object(obj) => {
            // First, resolve any $ref
            if let Some(ref reference) = obj.reference {
                let resolved = resolve_ref(root_schema, reference)?;
                return get_expected_type(root_schema, &resolved, segments, full_path);
            }

            // Handle subschemas (anyOf, oneOf, allOf) for Option<T> types
            if let Some(ref subschemas) = obj.subschemas {
                if let Some(ref any_of) = subschemas.any_of {
                    // For Option<T>, anyOf contains [T, null]
                    // Find the non-null type and traverse into it
                    let mut inner_result = None;
                    let mut has_null = false;

                    for sub in any_of {
                        if is_null_schema(sub) {
                            has_null = true;
                        } else {
                            // Try to get the type from the non-null branch
                            // If we have remaining segments, errors should propagate
                            inner_result =
                                Some(get_expected_type(root_schema, sub, segments, full_path));
                        }
                    }

                    if let Some(result) = inner_result {
                        // Propagate the result (either Ok or Err) from the inner traversal
                        return match result {
                            Ok(inner_type) if has_null => {
                                Ok(ExpectedType::Nullable(Box::new(inner_type)))
                            }
                            other => other,
                        };
                    }
                }
            }

            // If we're at the end of the path, determine the type from this schema
            if segments.is_empty() {
                return get_type_from_schema(obj);
            }

            let segment = segments[0];
            let remaining = &segments[1..];

            // Check if this is an object with properties
            if let Some(ref obj_validation) = obj.object {
                // Check if the segment matches a known property
                if let Some(prop_schema) = obj_validation.properties.get(segment) {
                    return get_expected_type(root_schema, prop_schema, remaining, full_path);
                }

                // Check for additionalProperties (for BTreeMap fields like `variables`)
                if let Some(ref additional) = obj_validation.additional_properties {
                    return get_expected_type(root_schema, additional, remaining, full_path);
                }

                // Property not found - suggest valid ones
                let valid_props: Vec<&str> = obj_validation
                    .properties
                    .keys()
                    .map(|s| s.as_str())
                    .collect();

                if valid_props.is_empty() {
                    bail!(
                        "Invalid path '{}': '{}' is not a valid property",
                        full_path,
                        segment
                    );
                } else {
                    // Check for "did you mean" suggestion
                    let suggestion = find_suggestion(segment, &valid_props);

                    if let Some(suggested) = suggestion {
                        bail!(
                            "Invalid path '{}': '{}' is not a valid property. Did you mean '{}'?",
                            full_path,
                            segment,
                            suggested,
                        );
                    } else {
                        bail!(
                            "Invalid path '{}': '{}' is not a valid property.",
                            full_path,
                            segment,
                        );
                    }
                }
            }

            // Check instance_type for null
            if let Some(ref instance_type) = obj.instance_type {
                if matches!(instance_type, SingleOrVec::Single(t) if **t == InstanceType::Null)
                    || matches!(instance_type, SingleOrVec::Vec(types) if types.contains(&InstanceType::Null))
                {
                    bail!(
                        "Invalid path '{}': '{}' cannot be accessed on null",
                        full_path,
                        segment
                    );
                }
            }

            bail!(
                "Invalid path '{}': cannot access '{}' on a primitive type",
                full_path,
                segment
            );
        }
        Schema::Bool(true) => Ok(ExpectedType::Any),
        Schema::Bool(false) => bail!("Invalid path '{}': schema disallows all values", full_path),
    }
}

/// Check if a schema represents the null type
fn is_null_schema(schema: &Schema) -> bool {
    match schema {
        Schema::Object(obj) => {
            matches!(
                &obj.instance_type,
                Some(SingleOrVec::Single(t)) if **t == InstanceType::Null
            )
        }
        _ => false,
    }
}

/// Extract the expected type from a SchemaObject
fn get_type_from_schema(obj: &schemars::schema::SchemaObject) -> Result<ExpectedType> {
    // First check for array with items schema
    if let Some(ref array_validation) = obj.array {
        let inner_type = if let Some(ref items) = array_validation.items {
            match items {
                SingleOrVec::Single(item_schema) => get_type_from_schema_ref(item_schema)?,
                SingleOrVec::Vec(schemas) => {
                    // Tuple-style array, just use first element type or Any
                    schemas
                        .first()
                        .map(get_type_from_schema_ref)
                        .transpose()?
                        .unwrap_or(ExpectedType::Any)
                }
            }
        } else {
            ExpectedType::Any
        };
        return Ok(ExpectedType::Array(Box::new(inner_type)));
    }

    if let Some(ref instance_type) = obj.instance_type {
        let types = match instance_type {
            SingleOrVec::Single(t) => vec![(**t)],
            SingleOrVec::Vec(types) => types.clone(),
        };

        // Filter out null to get the actual type
        let non_null: Vec<_> = types.iter().filter(|t| **t != InstanceType::Null).collect();
        let has_null = types.contains(&InstanceType::Null);

        let base_type = match non_null.first() {
            Some(InstanceType::String) => ExpectedType::String,
            Some(InstanceType::Integer) => ExpectedType::Integer,
            Some(InstanceType::Number) => ExpectedType::Number,
            Some(InstanceType::Boolean) => ExpectedType::Boolean,
            Some(InstanceType::Array) => {
                // Array type but no items schema - default to Any elements
                ExpectedType::Array(Box::new(ExpectedType::Any))
            }
            Some(InstanceType::Object) => ExpectedType::Object,
            Some(InstanceType::Null) | None => {
                if has_null {
                    return Ok(ExpectedType::Nullable(Box::new(ExpectedType::Any)));
                }
                return Ok(ExpectedType::Any);
            }
        };

        if has_null {
            Ok(ExpectedType::Nullable(Box::new(base_type)))
        } else {
            Ok(base_type)
        }
    } else if obj.object.is_some() {
        Ok(ExpectedType::Object)
    } else {
        Ok(ExpectedType::Any)
    }
}

/// Get expected type from a Schema reference (used for array items)
fn get_type_from_schema_ref(schema: &Schema) -> Result<ExpectedType> {
    match schema {
        Schema::Object(obj) => get_type_from_schema(obj),
        Schema::Bool(true) => Ok(ExpectedType::Any),
        Schema::Bool(false) => Ok(ExpectedType::Any), // Shouldn't happen, but fallback
    }
}

/// Find the best "did you mean" suggestion from valid options using similar crate
fn find_suggestion<'a>(input: &str, valid_options: &[&'a str]) -> Option<&'a str> {
    // get_close_matches returns up to n matches with similarity ratio >= cutoff
    // cutoff 0.6 is a reasonable threshold for typo detection
    let matches = get_close_matches(input, valid_options, 1, 0.6);
    matches.into_iter().next()
}

/// Resolve a $ref reference like "#/definitions/ServiceSource" to its Schema
fn resolve_ref(root_schema: &RootSchema, reference: &str) -> Result<Schema> {
    let prefix = "#/definitions/";
    if !reference.starts_with(prefix) {
        bail!("Unsupported reference format: {}", reference);
    }

    let type_name = &reference[prefix.len()..];
    root_schema
        .definitions
        .get(type_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Definition not found: {}", type_name))
}

/// Builds an EnvironmentConfig from a list of patch entries
pub fn build_config(entries: Vec<PatchEntry>) -> Result<EnvironmentConfig> {
    let mut json = serde_json::json!({});

    for (path, value) in entries {
        json.dot_set(&path, value)
            .with_context(|| format!("Failed to set path: {}", path))?;
    }

    serde_json::from_value(json).context("Failed to parse built config into EnvironmentConfig")
}

/// Checks if the EnvironmentConfig has any changes
pub fn is_empty(config: &EnvironmentConfig) -> bool {
    config.services.is_empty()
        && config.shared_variables.is_empty()
        && config.volumes.is_empty()
        && config.buckets.is_empty()
        && config.private_network_disabled.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_service_path_valid() {
        assert!(validate_service_path("source.image").is_ok());
        assert!(validate_service_path("deploy.startCommand").is_ok());
        assert!(validate_service_path("deploy.restartPolicyType").is_ok());
        assert!(validate_service_path("build.builder").is_ok());
        assert!(validate_service_path("variables.MY_VAR").is_ok());
        assert!(validate_service_path("variables.MY_VAR.value").is_ok());
    }

    #[test]
    fn test_validate_service_path_invalid() {
        let result = validate_service_path("invalid");
        assert!(result.is_err());

        let result = validate_service_path("deploy.invalidField");
        assert!(result.is_err());

        let result = validate_service_path("sorce.image"); // typo
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_service_value_string() {
        // String fields should accept strings
        let result = parse_service_value("source.image", "nginx:latest").unwrap();
        assert_eq!(result, serde_json::json!("nginx:latest"));

        let result = parse_service_value("deploy.startCommand", "npm start").unwrap();
        assert_eq!(result, serde_json::json!("npm start"));
    }

    #[test]
    fn test_parse_service_value_integer() {
        // Integer fields should parse numbers
        let result = parse_service_value("deploy.numReplicas", "3").unwrap();
        assert_eq!(result, serde_json::json!(3));

        let result = parse_service_value("deploy.healthcheckTimeout", "30").unwrap();
        assert_eq!(result, serde_json::json!(30));

        // Should reject non-integers
        let result = parse_service_value("deploy.numReplicas", "not-a-number");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_service_value_boolean() {
        // Boolean fields
        let result = parse_service_value("isDeleted", "true").unwrap();
        assert_eq!(result, serde_json::json!(true));

        let result = parse_service_value("isDeleted", "false").unwrap();
        assert_eq!(result, serde_json::json!(false));

        // Should reject non-booleans
        let result = parse_service_value("isDeleted", "not-a-bool");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_service_value_array() {
        // Array fields (like watch_patterns) - JSON syntax
        let result = parse_service_value("build.watchPatterns", r#"["src/**", "lib/**"]"#).unwrap();
        assert_eq!(result, serde_json::json!(["src/**", "lib/**"]));

        // Comma-separated syntax
        let result = parse_service_value("build.watchPatterns", "src/**,lib/**").unwrap();
        assert_eq!(result, serde_json::json!(["src/**", "lib/**"]));

        // Comma-separated with spaces
        let result = parse_service_value("build.watchPatterns", "src/**, lib/**, test/**").unwrap();
        assert_eq!(result, serde_json::json!(["src/**", "lib/**", "test/**"]));

        // Single value (no comma) becomes single-element array
        let result = parse_service_value("build.watchPatterns", "src/**").unwrap();
        assert_eq!(result, serde_json::json!(["src/**"]));

        // Empty elements are filtered out
        let result = parse_service_value("build.watchPatterns", "src/**,,lib/**").unwrap();
        assert_eq!(result, serde_json::json!(["src/**", "lib/**"]));
    }

    #[test]
    fn test_build_config_single_variable() {
        let entries = vec![(
            "services.abc123.variables.API_KEY".to_string(),
            serde_json::json!({"value": "secret"}),
        )];

        let config = build_config(entries).unwrap();
        assert!(config.services.contains_key("abc123"));
        let service = config.services.get("abc123").unwrap();
        assert!(service.variables.contains_key("API_KEY"));
    }

    #[test]
    fn test_build_config_multiple_entries() {
        let entries = vec![
            (
                "services.svc1.deploy.restartPolicyType".to_string(),
                serde_json::json!("ON_FAILURE"),
            ),
            (
                "services.svc1.deploy.restartPolicyMaxRetries".to_string(),
                serde_json::json!(5),
            ),
            (
                "services.svc1.source.image".to_string(),
                serde_json::json!("nginx:latest"),
            ),
        ];

        let config = build_config(entries).unwrap();
        let service = config.services.get("svc1").unwrap();

        let deploy = service.deploy.as_ref().unwrap();
        assert_eq!(deploy.restart_policy_type, Some("ON_FAILURE".to_string()));
        assert_eq!(deploy.restart_policy_max_retries, Some(5));

        let source = service.source.as_ref().unwrap();
        assert_eq!(source.image, Some("nginx:latest".to_string()));
    }

    #[test]
    fn test_is_empty() {
        let empty = EnvironmentConfig::default();
        assert!(is_empty(&empty));

        let entries = vec![(
            "services.abc.variables.FOO".to_string(),
            serde_json::json!({"value": "bar"}),
        )];
        let non_empty = build_config(entries).unwrap();
        assert!(!is_empty(&non_empty));
    }

    #[test]
    fn test_normalize_path() {
        // Leading dot
        assert_eq!(normalize_path(".deploy.numReplicas"), "deploy.numReplicas");

        // Trailing dot
        assert_eq!(normalize_path("deploy.numReplicas."), "deploy.numReplicas");

        // Both leading and trailing
        assert_eq!(normalize_path(".deploy.numReplicas."), "deploy.numReplicas");

        // Multiple consecutive dots
        assert_eq!(normalize_path("deploy..numReplicas"), "deploy.numReplicas");
        assert_eq!(normalize_path("deploy...numReplicas"), "deploy.numReplicas");

        // Mixed issues
        assert_eq!(
            normalize_path("..deploy..numReplicas.."),
            "deploy.numReplicas"
        );

        // Already normalized
        assert_eq!(normalize_path("deploy.numReplicas"), "deploy.numReplicas");

        // Single segment
        assert_eq!(normalize_path("deploy"), "deploy");
        assert_eq!(normalize_path(".deploy."), "deploy");
    }

    #[test]
    fn test_parse_service_value_with_path_normalization() {
        // Leading dot should work
        let result = parse_service_value(".deploy.numReplicas", "3").unwrap();
        assert_eq!(result, serde_json::json!(3));

        // Trailing dot should work
        let result = parse_service_value("deploy.numReplicas.", "3").unwrap();
        assert_eq!(result, serde_json::json!(3));

        // Multiple dots should work
        let result = parse_service_value("deploy..numReplicas", "3").unwrap();
        assert_eq!(result, serde_json::json!(3));
    }

    #[test]
    fn test_find_suggestion() {
        let options = vec!["numReplicas", "startCommand", "healthcheckPath"];

        // Close match (1 char missing)
        assert_eq!(find_suggestion("numReplica", &options), Some("numReplicas"));

        // Close match (case difference) - similar uses ratio-based matching
        assert_eq!(
            find_suggestion("numreplicas", &options),
            Some("numReplicas")
        );

        // Too different - no suggestion
        assert_eq!(find_suggestion("xyz", &options), None);
    }

    #[test]
    fn test_did_you_mean_in_error() {
        // Typo should trigger "did you mean"
        let result = parse_service_value("deploy.numReplica", "3"); // missing 's'
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Did you mean 'numReplicas'?"),
            "Error: {}",
            err
        );

        // Top-level typo
        let result = parse_service_value("deploi.numReplicas", "3"); // typo in 'deploy'
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Did you mean 'deploy'?"), "Error: {}", err);
    }

    #[test]
    fn test_multi_region_config_path() {
        // multiRegionConfig uses BTreeMap with arbitrary region keys
        let result =
            parse_service_value("deploy.multiRegionConfig.us-west2.numReplicas", "3").unwrap();
        assert_eq!(result, serde_json::json!(3));

        // Any region key should work (additionalProperties)
        let result =
            parse_service_value("deploy.multiRegionConfig.eu-central1.numReplicas", "5").unwrap();
        assert_eq!(result, serde_json::json!(5));
    }
}
