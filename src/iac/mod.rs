//! CLI-owned Infrastructure as Code engine.
//!
//! Language packages only author a project definition. This module evaluates
//! those files, diffs against the live environment, and applies ChangeSets.
//! `--runner` / `RAILWAY_IAC_TS_BIN` still invoke `railway-iac-ts` unchanged.

mod change_set;
mod compiler;
mod engine;
mod eval;
mod graph;
mod json;
pub mod ownership;
mod partial;
pub mod saved_plan;

#[allow(unused)]
pub use change_set::{ChangeSet, RAILWAY_CHANGE_SET_VERSION, diff_graphs, render_change_set};
#[allow(unused)]
pub use compiler::{
    CompileOptions, EnvironmentConfigToGraphOptions, environment_config_to_graph,
    graph_to_environment_config, project_definition_to_graph,
};
pub use engine::{NativeRun, run as run_native};
#[allow(unused)]
pub use eval::{EvalContext, EvaluatedFile, evaluate_file, evaluate_file_with_context};
#[allow(unused)]
pub use graph::{RAILWAY_GRAPH_VERSION, RailwayGraph, resource_address, validate_graph};
#[allow(unused)]
pub use partial::{needs_partial_claim_apply, parse_partial_name};

pub fn use_legacy_ts_runner(explicit_runner: Option<&str>) -> bool {
    if explicit_runner.is_some() {
        return true;
    }
    if std::env::var("RAILWAY_IAC_TS_BIN").is_ok() {
        return true;
    }
    matches!(
        std::env::var("RAILWAY_IAC_ENGINE").as_deref(),
        Ok("ts") | Ok("typescript") | Ok("legacy")
    )
}

#[cfg(test)]
mod tests;
