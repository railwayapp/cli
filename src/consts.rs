pub const fn get_user_agent() -> &'static str {
    concat!("CLI ", env!("CARGO_PKG_VERSION"))
}

pub const RAILWAY_TOKEN_ENV: &str = "RAILWAY_TOKEN";
pub const RAILWAY_API_TOKEN_ENV: &str = "RAILWAY_API_TOKEN";

pub const TICK_STRING: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ";
pub const NON_INTERACTIVE_FAILURE: &str = "This command is only available in interactive mode";
