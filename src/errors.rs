use reqwest::header::InvalidHeaderValue;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RailwayError {
    #[error("Unauthorized. Please login with `railway login`")]
    Unauthorized,

    #[error(
        "Unauthorized. Please check that your {0} is valid and has access to the resource you're trying to use."
    )]
    UnauthorizedToken(String),

    #[error("Unauthorized. Please run `railway login` again.")]
    UnauthorizedLogin,

    #[error(
        "Invalid {0}. Please check that it is valid and has access to the resource you're trying to use."
    )]
    InvalidRailwayToken(String),

    #[error("Login state is corrupt. Please logout and login back in.")]
    InvalidHeader(#[from] InvalidHeaderValue),

    #[error("Failed to get data from GraphQL response")]
    MissingResponseData,

    #[error("{0}")]
    GraphQLError(String),

    #[error("Failed to fetch: {0}")]
    FetchError(#[from] reqwest::Error),

    #[error("No linked project found. Run railway link to connect to a project")]
    NoLinkedProject,

    #[error(
        "Personal workspaces are no longer supported. Please specify a workspace by id or name"
    )]
    NoPersonalWorkspace,

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

    #[error("Project \"{0}\" was not found in the \"{1}\" workspace.")]
    ProjectNotFoundInWorkspace(String, String),

    #[error("Workspace \"{0}\" not found.")]
    WorkspaceNotFound(String),

    #[error("Service \"{0}\" not found.")]
    ServiceNotFound(String),

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

    #[error(
        "2FA is enabled. Use --2fa-code <CODE> to provide your verification code in non-interactive mode."
    )]
    TwoFactorRequiresInteractive,

    #[error("Two-factor authentication is required for workspace \"{0}\".\nEnable 2FA at: {1}")]
    TwoFactorEnforcementRequired(String, String),

    #[error("Could not find a variable to connect to the service with. Looking for \"{0}\".")]
    ConnectionVariableNotFound(String),

    #[error("Connection URL should point to the Railway TCP proxy")]
    InvalidConnectionVariable,

    #[error("You are being ratelimited. Please try again later")]
    Ratelimited,
}
