use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::json::field_str;

pub const RAILWAY_GRAPH_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RailwayGraph {
    pub version: u32,
    pub project: ProjectNode,
    #[serde(default)]
    pub environments: Vec<EnvironmentNode>,
    #[serde(default)]
    pub resources: Vec<Value>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// Project `variables` policy. Omitted from JSON when the file did not set it.
    #[serde(default, skip_serializing_if = "VariablePolicy::is_unspecified")]
    pub variables: VariablePolicy,
}

fn managed_by_default() -> bool {
    true
}

/// Which variable keys IaC owns. Absent in the file means managed, nothing ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VariablePolicy {
    #[serde(default = "managed_by_default")]
    pub managed: bool,
    #[serde(default)]
    pub ignore: Vec<String>,
    /// True when the authoring file omitted `variables`.
    #[serde(default)]
    pub default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for VariablePolicy {
    fn default() -> Self {
        Self {
            managed: true,
            ignore: Vec::new(),
            default: true,
            error: None,
        }
    }
}

impl VariablePolicy {
    pub fn is_unspecified(&self) -> bool {
        self.default && self.error.is_none() && self.managed && self.ignore.is_empty()
    }
}

/// Plan/apply summary of the policy against the live environment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct VariablePolicyReport {
    pub managed: bool,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub ignored_count: usize,
    #[serde(default)]
    pub by_pattern: BTreeMap<String, usize>,
}

/// One header line. `railway_variables` is every variable key on the current
/// graph; database template variables are not on that graph.
pub fn format_variable_policy_line(
    report: &VariablePolicyReport,
    railway_variables: usize,
) -> String {
    if !report.managed {
        return format!("variables: unmanaged \u{00b7} {railway_variables} on Railway not managed");
    }
    if report.ignored_count > 0 {
        let listed = report
            .ignore
            .iter()
            .filter_map(|pattern| {
                let count = report.by_pattern.get(pattern).copied().unwrap_or(0);
                (count > 0).then(|| format!("{pattern} {count}"))
            })
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "variables: managed \u{00b7} {} ignored on Railway ({listed})",
            report.ignored_count
        );
    }
    if report.default {
        return "variables: managed (default)".to_string();
    }
    "variables: managed".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProjectNode {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnvironmentNode {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

pub fn resource_address(resource_type: &str, name: &str) -> String {
    format!("{resource_type}.{name}")
}

pub fn resource_type(resource: &Value) -> &str {
    field_str(resource, "type").unwrap_or("")
}

pub fn resource_name(resource: &Value) -> &str {
    field_str(resource, "name").unwrap_or("")
}

pub fn resource_addr(resource: &Value) -> String {
    field_str(resource, "address")
        .map(str::to_string)
        .unwrap_or_else(|| resource_address(resource_type(resource), resource_name(resource)))
}

pub fn validate_graph(graph: &RailwayGraph) -> Vec<String> {
    let mut errors = Vec::new();
    if graph.version != RAILWAY_GRAPH_VERSION {
        errors.push(format!("Unsupported graph version: {}", graph.version));
    }
    let mut addresses = std::collections::HashSet::new();
    for resource in &graph.resources {
        let address = resource_addr(resource);
        if !addresses.insert(address.clone()) {
            errors.push(format!("Duplicate resource address: {address}"));
        }
    }
    for edge in &graph.edges {
        if !addresses.contains(&edge.from) {
            errors.push(format!("Edge references missing source: {}", edge.from));
        }
        if !addresses.contains(&edge.to) {
            errors.push(format!("Edge references missing target: {}", edge.to));
        }
    }
    errors
}
