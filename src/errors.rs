use reqwest::header::InvalidHeaderValue;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RailwayError {
    #[error("Unauthorized. Please login with `railway login`")]
    Unauthorized,

    #[error("Unauthorized")]
    InvalidRailwayToken,

    #[error("Login state is corrupt. Please logout and login back in.")]
    InvalidHeader(#[from] InvalidHeaderValue),

    #[error("Failed to get data from GraphQL response")]
    MissingResponseData,

    #[error("{0}")]
    GraphQLError(String),

    #[error("Failed to fetch: {0}")]
    FetchError(#[from] reqwest::Error),

    #[error("No linked project found. Run railway link to connect to a project, and a service.")]
    NoLinkedProject,

    #[error("Project not found. Run `railway link` to connect to a project.")]
    ProjectNotFound,

    #[error("Project is deleted. Run `railway link` to connect to a project.")]
    ProjectDeleted,

    #[error("Environment is deleted. Run `railway environment` to connect to an environment.")]
    EnvironmentDeleted,

    #[error("No projects found. Run `railway init` to create a new project")]
    NoProjects,

    #[error("Project does not have any services")]
    NoServices,

    #[error(
        "Environment \"{0}\" not found.\nRun `railway environment` to connect to an environment."
    )]
    EnvironmentNotFound(String),

    #[error("Project \"{0}\" was not found in the \"{1}\" team.")]
    ProjectNotFoundInTeam(String, String),

    #[error("Service \"{0}\" not found.\nRun `railway service` to connect to a service.")]
    ServiceNotFound(String),

    #[error("Team \"{0}\" not found.")]
    TeamNotFound(String),

    #[error("Project has no services.")]
    ProjectHasNoServices,

    #[error("No service linked\nRun `railway service` to link a service")]
    NoServiceLinked,

    #[error("No command provided. Run with `railway run <cmd>`")]
    NoCommandProvided,

    #[error("{0}")]
    FailedToUpload(String),

    #[error("Volume {0} not found.")]
    VolumeNotFound(String),

    #[error("2FA code is incorrect. Please try again.")]
    InvalidTwoFactorCode,

    #[error("Could not find a variable to connect to the service with. Looking for \"{0}\".")]
    ConnectionVariableNotFound(String),

    #[error("Connection URL should point to the Railway TCP proxy")]
    InvalidConnectionVariable,
}
