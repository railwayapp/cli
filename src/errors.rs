use reqwest::header::InvalidHeaderValue;
use thiserror::Error;

/// Substrings that identify an authorization failure in a *rendered* error
/// message.
///
/// The MCP tool layer flattens `RailwayError` into an opaque `McpError` string
/// before its dispatcher sees it, so recovery there has to match on text. These
/// markers are declared next to the variants they describe, and
/// `auth_failure_markers_cover_unauthorized_variants` fails if a variant's
/// message stops containing one — otherwise rewording an `#[error(...)]` would
/// silently disable mid-session re-auth.
pub const AUTH_FAILURE_MARKERS: &[&str] = &["Unauthorized", "Not authenticated"];

/// `Retry-After` turned into something worth reading. The server sends it on
/// every 429; without it the only honest thing to say is "later".
fn ratelimit_message(retry_after_secs: Option<u64>) -> String {
    match retry_after_secs {
        Some(secs) if secs > 90 => format!(
            "You are being ratelimited. Try again in about {} minutes",
            secs.div_ceil(60)
        ),
        Some(secs) => format!("You are being ratelimited. Try again in {secs} seconds"),
        None => "You are being ratelimited. Please try again later".to_string(),
    }
}

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

    #[error(
        "Environment \"{0}\" is restricted. Ask a workspace admin for access, or choose an unrestricted environment."
    )]
    EnvironmentRestricted(String),

    #[error("No projects found. Run `railway init` to create a new project")]
    NoProjects,

    #[error(
        "Environment \"{0}\" not found.\nRun `railway environment` to connect to an environment."
    )]
    EnvironmentNotFound(String),

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

    #[error("Bucket \"{0}\" not found.")]
    BucketNotFound(String),

    #[error("Bucket \"{0}\" is not deployed in environment \"{1}\".")]
    BucketNotInEnvironment(String, String),

    #[error("2FA code is incorrect. Please try again.")]
    InvalidTwoFactorCode,

    #[error("Two-factor authentication is required for workspace \"{0}\".\nEnable 2FA at: {1}")]
    TwoFactorEnforcementRequired(String, String),

    #[error("Could not find a variable to connect to the service with. Looking for \"{0}\".")]
    ConnectionVariableNotFound(String),

    #[error("Connection URL should point to the Railway TCP proxy")]
    InvalidConnectionVariable,

    #[error("{}", ratelimit_message(*retry_after_secs))]
    Ratelimited { retry_after_secs: Option<u64> },

    #[error("Device code expired. Please run `railway login` again.")]
    OAuthDeviceCodeExpired,

    #[error("Authorization was denied by the user.")]
    OAuthAccessDenied,

    /// Transient refresh failure: the stored credentials are still valid, so
    /// this must NOT tell the user to log in again. A backboard blip used to
    /// send everyone who refreshed during it to `railway login` for nothing.
    #[error("Couldn't refresh your session: {0}. This is usually temporary — please try again.")]
    OAuthRefreshFailed(String),

    /// The server rejected the stored refresh token with `invalid_grant` —
    /// it was revoked, expired, or never existed. Unlike every other refresh
    /// failure this is *permanent*: retrying with the same credential can
    /// never succeed, so the caller clears it instead of looping forever.
    #[error("Your saved login is no longer valid ({0}). Please run `railway login` again.")]
    OAuthInvalidGrant(String),

    #[error("OAuth error: {0}")]
    OAuthError(String),

    #[error("Not signed in.")]
    NotAuthenticated,
}

impl RailwayError {
    /// Stable, machine-readable error code for structured (`--json`)
    /// output. Exhaustive on purpose: adding a variant forces a code
    /// decision rather than silently falling into a generic bucket.
    pub fn code(&self) -> &'static str {
        match self {
            RailwayError::Unauthorized
            | RailwayError::UnauthorizedToken(_)
            | RailwayError::UnauthorizedLogin => "UNAUTHORIZED",
            RailwayError::InvalidRailwayToken(_) => "INVALID_TOKEN",
            RailwayError::InvalidHeader(_) => "INVALID_AUTH_STATE",
            RailwayError::MissingResponseData => "MISSING_RESPONSE_DATA",
            RailwayError::GraphQLError(_) => "GRAPHQL_ERROR",
            RailwayError::FetchError(_) => "FETCH_ERROR",
            RailwayError::NoLinkedProject => "NO_LINKED_PROJECT",
            RailwayError::NoPersonalWorkspace => "NO_PERSONAL_WORKSPACE",
            RailwayError::ProjectNotFound => "PROJECT_NOT_FOUND",
            RailwayError::ProjectDeleted => "PROJECT_DELETED",
            RailwayError::EnvironmentDeleted => "ENVIRONMENT_DELETED",
            RailwayError::EnvironmentRestricted(_) => "ENVIRONMENT_RESTRICTED",
            RailwayError::NoProjects => "NO_PROJECTS",
            RailwayError::EnvironmentNotFound(_) => "ENVIRONMENT_NOT_FOUND",
            RailwayError::WorkspaceNotFound(_) => "WORKSPACE_NOT_FOUND",
            RailwayError::ServiceNotFound(_) => "SERVICE_NOT_FOUND",
            RailwayError::ProjectHasNoServices => "PROJECT_HAS_NO_SERVICES",
            RailwayError::NoServiceLinked => "NO_SERVICE_LINKED",
            RailwayError::NoCommandProvided => "NO_COMMAND_PROVIDED",
            RailwayError::FailedToUpload(_) => "UPLOAD_FAILED",
            RailwayError::VolumeNotFound(_) => "VOLUME_NOT_FOUND",
            RailwayError::BucketNotFound(_) => "BUCKET_NOT_FOUND",
            RailwayError::BucketNotInEnvironment(_, _) => "BUCKET_NOT_IN_ENVIRONMENT",
            RailwayError::InvalidTwoFactorCode => "INVALID_2FA_CODE",
            RailwayError::TwoFactorEnforcementRequired(_, _) => "TWO_FACTOR_REQUIRED",
            RailwayError::ConnectionVariableNotFound(_) => "CONNECTION_VARIABLE_NOT_FOUND",
            RailwayError::InvalidConnectionVariable => "INVALID_CONNECTION_VARIABLE",
            RailwayError::Ratelimited { .. } => "RATELIMITED",
            RailwayError::OAuthDeviceCodeExpired => "OAUTH_DEVICE_CODE_EXPIRED",
            RailwayError::OAuthAccessDenied => "OAUTH_ACCESS_DENIED",
            RailwayError::OAuthRefreshFailed(_) => "OAUTH_REFRESH_FAILED",
            RailwayError::OAuthInvalidGrant(_) => "OAUTH_INVALID_GRANT",
            RailwayError::OAuthError(_) => "OAUTH_ERROR",
            RailwayError::NotAuthenticated => "NOT_AUTHENTICATED",
        }
    }

    /// Optional actionable next step, surfaced as a `hint` field in
    /// structured output. Opt-in per variant — most variants already
    /// embed guidance in their message.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            RailwayError::NotAuthenticated | RailwayError::OAuthInvalidGrant(_) => {
                Some("Run `railway login` to authenticate, then re-run.")
            }
            RailwayError::NoLinkedProject | RailwayError::ProjectNotFound => {
                Some("Run `railway link` to connect to a project.")
            }
            RailwayError::NoServiceLinked => Some("Run `railway service` to link a service."),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant that means "the server refused this on authorization" must
    /// be recognisable from its rendered message alone.
    #[test]
    fn auth_failure_markers_cover_unauthorized_variants() {
        let variants = [
            RailwayError::Unauthorized,
            RailwayError::UnauthorizedLogin,
            RailwayError::UnauthorizedToken("RAILWAY_TOKEN".to_string()),
        ];
        for variant in variants {
            let rendered = variant.to_string();
            assert!(
                AUTH_FAILURE_MARKERS.iter().any(|m| rendered.contains(m)),
                "no AUTH_FAILURE_MARKERS entry matches {rendered:?}; mid-session re-auth would stop working for this variant"
            );
        }
    }
}
