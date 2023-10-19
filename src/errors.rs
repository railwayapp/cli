use hyper::header::InvalidHeaderValue;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RailwayError {
    #[error("Unauthorized. Please login with `railway login`")]
    Unauthorized,

    #[error("Login state is corrupt. Please logout and login back in.")]
    InvalidHeader(#[from] InvalidHeaderValue),

    #[error("Failed to get data from GraphQL response")]
    MissingResponseData,

    #[error("{0}")]
    GraphQLError(String),

    #[error("Failed to fetch: {0}")]
    FetchError(#[from] reqwest::Error),

    #[error("No linked project found. Run `railway link` to connect to a project.")]
    NoLinkedProject,

    #[error("Project not found. Run `railway link` to connect to a project.")]
    ProjectNotFound,

    #[error("No projects found. Run `railway init` to create a new project")]
    NoProjects,

    #[error("Project does not have any services")]
    NoServices,

    #[error(
        "Environment \"{0}\" not found.\nRun `railway environment` to connect to an environment."
    )]
    EnvironmentNotFound(String),

    #[error("Plugin \"{0}\" not found.")]
    PluginNotFound(String),

    #[error("Service \"{0}\" not found.\nRun `railway service` to connect to a service.")]
    ServiceNotFound(String),

    #[error("Service or plugin \"{0}\" not found.")]
    ServiceOrPluginNotFound(String),

    #[error("Project has no services or plugins.")]
    ProjectHasNoServicesOrPlugins,

    #[error("No service linked and no plugins found\nRun `railway service` to link a service")]
    NoServiceLinked,

    #[error("2FA code is incorrect. Please try again.")]
    InvalidTwoFactorCode,

    #[error("No command provided. Run with `railway run <cmd>`")]
    NoCommandProvided,

    #[error("{0}")]
    FailedToUpload(String),

    #[error("Could not determine database type for service {0}")]
    UnknownDatabaseType(String),
}
