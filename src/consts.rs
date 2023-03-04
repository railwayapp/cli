pub const fn get_user_agent() -> &'static str {
    concat!("CLI ", env!("CARGO_PKG_VERSION"))
}

pub const TICK_STRING: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ";

pub const PLUGINS: &[&str] = &["PostgreSQL", "MySQL", "Redis", "MongoDB"];

pub const NO_SERVICE_LINKED: &str =
    "No service linked and no plugins found\nRun `railway service` to link a service";
pub const ABORTED_BY_USER: &str = "Aborted by user";
pub const PROJECT_NOT_FOUND: &str = "Project not found!";
pub const SERVICE_NOT_FOUND: &str = "Service not found!";
pub const NON_INTERACTIVE_FAILURE: &str = "This command is only available in interactive mode";
