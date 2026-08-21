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
