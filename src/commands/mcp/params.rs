use std::collections::BTreeMap;

use rmcp::schemars;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectParams {
    /// The project ID to use. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ServiceParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListDeploymentsParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Maximum number of deployments to return (default: 20).
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogType {
    Build,
    #[default]
    Deploy,
    Http,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetLogsParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Specific deployment ID to get logs for. If omitted, uses the latest deployment.
    #[serde(default)]
    pub deployment_id: Option<String>,
    /// Type of logs: "build", "deploy", or "http" (default: "deploy").
    #[serde(default)]
    pub log_type: Option<LogType>,
    /// Number of log lines to return (default: 100).
    #[serde(default)]
    pub lines: Option<i64>,
    /// Start time filter. Supports relative ("30m", "2h", "1d") or ISO 8601 format.
    #[serde(default)]
    pub since: Option<String>,
    /// End time filter. Supports relative ("30m", "2h", "1d") or ISO 8601 format.
    #[serde(default)]
    pub until: Option<String>,
    /// Filter by log level: "error", "warn", or "info" (for build/deploy logs).
    #[serde(default)]
    pub level: Option<String>,
    /// Search string to filter logs (for build/deploy logs).
    #[serde(default)]
    pub search: Option<String>,
    /// Filter HTTP logs by request method: GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS (requires log_type: "http").
    #[serde(default)]
    pub method: Option<String>,
    /// Filter HTTP logs by status code. Accepts: exact (200), comparison (>=400), or range (500..599) (requires log_type: "http").
    #[serde(default)]
    pub status: Option<String>,
    /// Filter HTTP logs by request path, e.g. "/api/users" (requires log_type: "http").
    #[serde(default)]
    pub path: Option<String>,
    /// Filter HTTP logs by request ID (requires log_type: "http").
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetVariablesParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Map of variable names to values to set.
    pub variables: BTreeMap<String, String>,
    /// If true, skip triggering redeploys after setting variables.
    #[serde(default)]
    pub skip_deploys: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GenerateDomainParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Custom domain to add (e.g. "api.example.com"). If omitted, generates a Railway service domain.
    #[serde(default)]
    pub domain: Option<String>,
    /// Target port for the domain.
    #[serde(default)]
    pub port: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LinkServiceParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID to link. If omitted along with service_name, lists available services.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The service name to link. Alternative to service_id.
    #[serde(default)]
    pub service_name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LinkEnvironmentParams {
    /// The environment ID to link. If omitted along with environment_name, lists available environments.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// The environment name to link. Alternative to environment_id.
    #[serde(default)]
    pub environment_name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeployParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the linked service or backboard auto-creates one.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Path to the directory to deploy. Defaults to current directory.
    #[serde(default)]
    pub path: Option<String>,
    /// Message to attach to the deployment.
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateProjectParams {
    /// The name for the new project.
    pub name: String,
    /// Optional description for the project.
    #[serde(default)]
    pub description: Option<String>,
    /// Workspace ID to create the project in. If omitted, uses the user's personal workspace.
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateEnvironmentParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The name for the new environment.
    pub name: String,
    /// Source environment ID to fork from.
    #[serde(default)]
    pub source_environment_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateServiceParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Name for the new service.
    #[serde(default)]
    pub name: Option<String>,
    /// GitHub repo to connect (e.g. "owner/repo").
    #[serde(default)]
    pub source_repo: Option<String>,
    /// Docker image to use (e.g. "nginx:latest").
    #[serde(default)]
    pub source_image: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveServiceParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Must be set to true to confirm deletion. This action is irreversible.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateServiceParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Build command override.
    #[serde(default)]
    pub build_command: Option<String>,
    /// Start command override.
    #[serde(default)]
    pub start_command: Option<String>,
    /// Number of replicas.
    #[serde(default)]
    pub num_replicas: Option<i64>,
    /// Health check path (e.g. "/health").
    #[serde(default)]
    pub health_check_path: Option<String>,
    /// Whether to sleep the service when inactive.
    #[serde(default)]
    pub sleep_application: Option<bool>,
    /// Root directory for the build.
    #[serde(default)]
    pub root_directory: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EnvironmentStatusParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetServiceConfigParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReferenceVariable {
    /// Variable name.
    pub name: String,
    /// Reference value (must start with "${{").
    pub value: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddReferenceVariableParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Variables to set, each with a name and a reference value starting with "${{".
    pub variables: Vec<ReferenceVariable>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeployTemplateParams {
    /// Template code to deploy (e.g. "postgres", "redis").
    pub template_code: String,
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchTemplatesParams {
    /// Search query to match against template names and codes.
    pub query: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ServiceMetricsParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Metrics to fetch: CPU_USAGE, MEMORY_USAGE_GB, DISK_USAGE_GB, NETWORK_RX_GB, NETWORK_TX_GB. Defaults to CPU_USAGE and MEMORY_USAGE_GB.
    #[serde(default)]
    pub measurements: Option<Vec<String>>,
    /// Number of hours back to query (default: 1).
    #[serde(default)]
    pub hours_back: Option<i64>,
    /// Sample rate in seconds (default: 60).
    #[serde(default)]
    pub sample_rate_seconds: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct HttpObservabilityParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Specific deployment ID. If omitted, uses the latest deployment.
    #[serde(default)]
    pub deployment_id: Option<String>,
    /// Number of log entries to sample (default: 200).
    #[serde(default)]
    pub lines: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateBucketParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Optional name for the bucket.
    #[serde(default)]
    pub name: Option<String>,
    /// Region: sjc, iad, ams, or sin (default: sjc).
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveBucketParams {
    /// The bucket ID to remove.
    pub bucket_id: String,
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Must be set to true to confirm deletion. This action is irreversible.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateVolumeParams {
    /// Mount path for the volume (e.g. "/data").
    pub mount_path: String,
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateVolumeParams {
    /// The volume ID to update.
    pub volume_id: String,
    /// New mount path.
    #[serde(default)]
    pub mount_path: Option<String>,
    /// New name for the volume.
    #[serde(default)]
    pub name: Option<String>,
    /// The environment ID (required when updating mount_path).
    #[serde(default)]
    pub environment_id: Option<String>,
    /// The service ID (used when updating mount_path).
    #[serde(default)]
    pub service_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveVolumeParams {
    /// The volume ID to remove.
    pub volume_id: String,
    /// Must be set to true to confirm deletion. This action is irreversible.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DocsSearchParams {
    /// Search query to find Railway documentation pages.
    pub query: String,
}
