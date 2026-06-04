pub const fn get_user_agent() -> &'static str {
    concat!("CLI ", env!("CARGO_PKG_VERSION"))
}

pub const RAILWAY_TOKEN_ENV: &str = "RAILWAY_TOKEN";
pub const RAILWAY_API_TOKEN_ENV: &str = "RAILWAY_API_TOKEN";
pub const RAILWAY_PROJECT_ID_ENV: &str = "RAILWAY_PROJECT_ID";
pub const RAILWAY_ENVIRONMENT_ID_ENV: &str = "RAILWAY_ENVIRONMENT_ID";
pub const RAILWAY_SERVICE_ID_ENV: &str = "RAILWAY_SERVICE_ID";
pub const RAILWAY_CALLER_ENV: &str = "RAILWAY_CALLER";
pub const RAILWAY_AGENT_SESSION_ENV: &str = "RAILWAY_AGENT_SESSION";
pub const RAILWAY_INSTALL_REQUEST_ID_ENV: &str = "RAILWAY_INSTALL_REQUEST_ID";
pub const RAILWAY_STAGE_UPDATE_ENV: &str = "_RAILWAY_STAGE_UPDATE";
pub const RAILWAY_UPDATE_SKILLS_ENV: &str = "_RAILWAY_UPDATE_SKILLS";
pub const RAILWAY_HTTP_TIMEOUT_ENV: &str = "RAILWAY_HTTP_TIMEOUT";

/// Default HTTP request timeout in seconds, used when `RAILWAY_HTTP_TIMEOUT` is unset.
/// Long-running mutations (e.g. duplicating a multi-service environment with volumes)
/// can exceed the previous 30s cap, so the default is generous and overridable.
pub const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 90;

pub const TICK_STRING: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ";
