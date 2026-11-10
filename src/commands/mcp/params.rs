use std::collections::BTreeMap;
use std::fmt;

use rmcp::schemars;
use serde::de::{self, Deserializer, Visitor};

/// Deserialize a boolean that agents sometimes send as a string.
///
/// Models routinely emit `"confirm": "true"` instead of `true`, which serde
/// rejects with `-32602 invalid type: string "true", expected a boolean`.
/// Telemetry shows this on remove_tcp_proxy, delete_domain, remove_service,
/// remove_volume and remove_bucket, and it is worst on `confirm` because that
/// field is deliberately left out of the advertised schema (see the
/// `#[schemars(skip)]` on each one), so the model has to guess its type.
///
/// Accepts a JSON boolean or the strings "true"/"false" in any case, with
/// surrounding whitespace ignored. Anything else is still an error: a
/// destructive confirmation must never be inferred from an ambiguous value.
///
/// `deserialize_with` does not touch the schema schemars derives, so the
/// advertised type stays `boolean`.
fn lenient_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    deserializer.deserialize_any(LenientBool)
}

/// [`lenient_bool`] for an optional field; `null` and absence stay `None`.
fn lenient_opt_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    deserializer.deserialize_option(LenientOptBool)
}

struct LenientBool;

impl<'de> Visitor<'de> for LenientBool {
    type Value = bool;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a boolean or the string \"true\" or \"false\"")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<bool, E> {
        Ok(v)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<bool, E> {
        let trimmed = v.trim();
        if trimmed.eq_ignore_ascii_case("true") {
            Ok(true)
        } else if trimmed.eq_ignore_ascii_case("false") {
            Ok(false)
        } else {
            Err(E::invalid_value(de::Unexpected::Str(v), &self))
        }
    }
}

struct LenientOptBool;

impl<'de> Visitor<'de> for LenientOptBool {
    type Value = Option<bool>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        LenientBool.expecting(f)
    }

    fn visit_none<E: de::Error>(self) -> Result<Option<bool>, E> {
        Ok(None)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Option<bool>, E> {
        Ok(None)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Option<bool>, D::Error> {
        lenient_bool(deserializer).map(Some)
    }
}

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
pub struct PrivateNetworkParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Private network name, ID, or DNS name. If omitted, status returns all networks.
    #[serde(default)]
    pub network: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdatePrivateNetworkParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Private network name, ID, or DNS name. If omitted, uses the only network or the network named "railway".
    #[serde(default)]
    pub network: Option<String>,
    /// Endpoint name prefix, without the .internal suffix.
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateTcpProxyParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Application port to expose through the TCP proxy.
    pub application_port: i64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TcpProxySelectorParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// TCP proxy ID, domain, endpoint, proxy port, or application port.
    pub proxy: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveTcpProxyParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// TCP proxy ID, domain, endpoint, proxy port, or application port.
    pub proxy: String,
    /// Must be set to true to confirm deletion. This action is irreversible.
    #[serde(default, deserialize_with = "lenient_bool")]
    #[schemars(skip)]
    pub confirm: bool,
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
    #[serde(default, deserialize_with = "lenient_opt_bool")]
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
pub struct DomainStatusParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Domain name, URL, or domain ID.
    pub domain: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteDomainParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Domain name, URL, or domain ID.
    pub domain: String,
    /// Must be set to true to confirm deletion. This action is irreversible.
    #[serde(default, deserialize_with = "lenient_bool")]
    #[schemars(skip)]
    pub confirm: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateDomainParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Domain name, URL, or domain ID.
    pub domain: String,
    /// Target port for the domain. Must be from 1 to 65535.
    #[serde(default)]
    pub port: Option<i64>,
    /// Rename a Railway-provided service domain. Accepts a full service domain or host label.
    #[serde(default)]
    pub new_domain: Option<String>,
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
    /// Git branch to deploy when source_repo is set. Defaults to the repo default branch when Railway can see it.
    #[serde(default)]
    pub branch: Option<String>,
    /// Docker image to use (e.g. "nginx:latest").
    #[serde(default)]
    pub source_image: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ConnectServiceSourceParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// GitHub repo to connect, in owner/repo format.
    #[serde(default)]
    pub source_repo: Option<String>,
    /// Git branch to deploy when source_repo is set. Defaults to the repo default branch when Railway can see it.
    #[serde(default)]
    pub branch: Option<String>,
    /// Docker image to connect (e.g. "nginx:latest").
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
    #[serde(default, deserialize_with = "lenient_bool")]
    #[schemars(skip)]
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
    /// Legacy single-region replica count. Prefer scale_service for replica changes.
    #[serde(default)]
    pub num_replicas: Option<i64>,
    /// Health check path (e.g. "/health").
    #[serde(default)]
    pub health_check_path: Option<String>,
    /// Health check timeout in milliseconds.
    #[serde(default)]
    pub healthcheck_timeout: Option<i64>,
    /// Whether to sleep the service when inactive.
    #[serde(default, deserialize_with = "lenient_opt_bool")]
    pub sleep_application: Option<bool>,
    /// Root directory for the build.
    #[serde(default)]
    pub root_directory: Option<String>,
    /// Cron schedule expression (e.g. "0 */5 * * *").
    #[serde(default)]
    pub cron_schedule: Option<String>,
    /// Path to the Dockerfile (e.g. "Dockerfile.prod").
    #[serde(default)]
    pub dockerfile_path: Option<String>,
    /// Restart policy type: "ALWAYS", "ON_FAILURE", or "NEVER".
    #[serde(default)]
    pub restart_policy_type: Option<String>,
    /// Maximum number of restart retries (used with ON_FAILURE restart policy).
    #[serde(default)]
    pub restart_policy_max_retries: Option<i64>,
    /// Commands to run before deploying (e.g. database migrations).
    #[serde(default)]
    pub pre_deploy_command: Option<Vec<String>>,
    /// Legacy deploy region ID. Prefer scale_service for region replica changes.
    #[serde(default)]
    pub region: Option<String>,
    /// Path to the Railway config file.
    #[serde(default)]
    pub railway_config_file: Option<String>,
    /// File watch patterns that trigger deploys.
    #[serde(default)]
    pub watch_patterns: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScaleServiceParams {
    /// The project ID. If omitted, uses the currently linked project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The service ID or name. If omitted, uses the currently linked service.
    #[serde(default)]
    pub service_id: Option<String>,
    /// The environment ID or name. If omitted, uses the currently linked environment.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Map of friendly region names or region IDs to replica counts, e.g. {"eu-west": 2, "us-east": 1}. Maximum 50 total replicas across regions.
    pub replicas: BTreeMap<String, i64>,
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
    /// Search query to send to Railway template search.
    pub query: String,
    /// Maximum number of templates to return. Defaults to 5.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Cursor from pageInfo.endCursor for fetching the next page.
    #[serde(default)]
    pub after: Option<String>,
    /// Optional template category filter.
    #[serde(default)]
    pub category: Option<String>,
    /// Optional verification-state filter.
    #[serde(default, deserialize_with = "lenient_opt_bool")]
    pub verified: Option<bool>,
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
    #[serde(default, deserialize_with = "lenient_bool")]
    #[schemars(skip)]
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
    #[serde(default, deserialize_with = "lenient_bool")]
    #[schemars(skip)]
    pub confirm: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DocsSearchParams {
    /// Search query to find Railway documentation pages.
    pub query: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DocsFetchParams {
    /// The documentation page URL (e.g. "https://docs.railway.com/guides/getting-started") or slug (e.g. "guides/getting-started").
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::handler::server::common::schema_for_type;
    use serde_json::{Value, json};

    fn parse<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, serde_json::Error> {
        serde_json::from_value(args)
    }

    #[test]
    fn confirm_accepts_booleans_and_boolean_strings() {
        for (value, expected) in [
            (json!(true), true),
            (json!(false), false),
            (json!("true"), true),
            (json!("false"), false),
            (json!("TRUE"), true),
            (json!("False"), false),
            (json!(" true "), true),
        ] {
            let p: RemoveTcpProxyParams =
                parse(json!({ "proxy": "p", "confirm": value.clone() })).unwrap();
            assert_eq!(p.confirm, expected, "confirm: {value}");
        }
    }

    #[test]
    fn confirm_defaults_to_false_when_absent() {
        let p: DeleteDomainParams = parse(json!({ "domain": "example.com" })).unwrap();
        assert!(!p.confirm);
    }

    #[test]
    fn confirm_rejects_ambiguous_values() {
        // A destructive confirmation is never inferred from something that is
        // not plainly true/false.
        for value in [json!("yes"), json!("1"), json!(1), json!(""), json!(null)] {
            let err = parse::<RemoveVolumeParams>(json!({ "volume_id": "v", "confirm": value }))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("a boolean or the string"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn every_confirm_field_is_lenient() {
        // The five destructive tools telemetry shows failing on string "true".
        let p: RemoveTcpProxyParams = parse(json!({ "proxy": "p", "confirm": "true" })).unwrap();
        assert!(p.confirm);
        let p: DeleteDomainParams =
            parse(json!({ "domain": "example.com", "confirm": "true" })).unwrap();
        assert!(p.confirm);
        let p: RemoveServiceParams = parse(json!({ "confirm": "true" })).unwrap();
        assert!(p.confirm);
        let p: RemoveBucketParams = parse(json!({ "bucket_id": "b", "confirm": "true" })).unwrap();
        assert!(p.confirm);
        let p: RemoveVolumeParams = parse(json!({ "volume_id": "v", "confirm": "true" })).unwrap();
        assert!(p.confirm);
    }

    #[test]
    fn optional_bools_accept_strings_and_keep_null_as_none() {
        let p: SetVariablesParams =
            parse(json!({ "variables": {}, "skip_deploys": "TRUE" })).unwrap();
        assert_eq!(p.skip_deploys, Some(true));
        let p: SetVariablesParams =
            parse(json!({ "variables": {}, "skip_deploys": false })).unwrap();
        assert_eq!(p.skip_deploys, Some(false));
        let p: SetVariablesParams =
            parse(json!({ "variables": {}, "skip_deploys": null })).unwrap();
        assert_eq!(p.skip_deploys, None);
        let p: SetVariablesParams = parse(json!({ "variables": {} })).unwrap();
        assert_eq!(p.skip_deploys, None);

        let p: UpdateServiceParams = parse(json!({ "sleep_application": "false" })).unwrap();
        assert_eq!(p.sleep_application, Some(false));
        let p: SearchTemplatesParams = parse(json!({ "query": "pg", "verified": "true" })).unwrap();
        assert_eq!(p.verified, Some(true));

        assert!(
            parse::<SearchTemplatesParams>(json!({ "query": "pg", "verified": "maybe" })).is_err(),
            "non-boolean strings must still be rejected"
        );
    }

    fn property(schema: &serde_json::Map<String, Value>, name: &str) -> Option<Value> {
        schema.get("properties").and_then(|p| p.get(name)).cloned()
    }

    #[test]
    fn advertised_schema_is_unchanged_by_lenient_parsing() {
        // `confirm` stays hidden: advertising it is what led agents to set it
        // on the first call and skip the destructive-action preview.
        for schema in [
            schema_for_type::<RemoveTcpProxyParams>(),
            schema_for_type::<DeleteDomainParams>(),
            schema_for_type::<RemoveServiceParams>(),
            schema_for_type::<RemoveBucketParams>(),
            schema_for_type::<RemoveVolumeParams>(),
        ] {
            assert_eq!(property(&schema, "confirm"), None);
        }

        // The advertised optional booleans are still plain booleans, not a
        // string/any union.
        let boolean = json!(["boolean", "null"]);
        for (schema, field) in [
            (schema_for_type::<SetVariablesParams>(), "skip_deploys"),
            (
                schema_for_type::<UpdateServiceParams>(),
                "sleep_application",
            ),
            (schema_for_type::<SearchTemplatesParams>(), "verified"),
        ] {
            let prop = property(&schema, field).unwrap();
            assert_eq!(prop["type"], boolean, "{field}: {prop}");
        }
    }
}
