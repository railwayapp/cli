pub const fn get_user_agent() -> &'static str {
    concat!("CLI ", env!("CARGO_PKG_VERSION"))
}

pub const RAILWAY_TOKEN_ENV: &str = "RAILWAY_TOKEN";
pub const RAILWAY_API_TOKEN_ENV: &str = "RAILWAY_API_TOKEN";
pub const RAILWAY_PROJECT_ID_ENV: &str = "RAILWAY_PROJECT_ID";
pub const RAILWAY_ENVIRONMENT_ID_ENV: &str = "RAILWAY_ENVIRONMENT_ID";
pub const RAILWAY_SERVICE_ID_ENV: &str = "RAILWAY_SERVICE_ID";
pub const RAILWAY_STAGE_UPDATE_ENV: &str = "_RAILWAY_STAGE_UPDATE";

pub const TICK_STRING: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ";
pub const NON_INTERACTIVE_FAILURE: &str = "This command is only available in interactive mode";
