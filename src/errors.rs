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

    #[error("Project not found. Run `railway link` to connect to a project.")]
    ProjectNotFound,

    #[error("Project does not have any services")]
    NoServices,

    #[error("Environment {0} not found. Run `railway environment` to connect to an environment.")]
    EnvironmentNotFound(String),
}
