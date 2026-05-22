use anyhow::anyhow;
use reqwest::Client;

use super::*;
use crate::{
    controllers::{
        dash_tui::{DashTuiParams, DashboardAuthMode},
        environment::get_matched_environment,
        project::get_project,
        user::get_user,
    },
    errors::RailwayError,
};

const DASHBOARD_LOGIN_ERROR: &str = "Not logged in. Run railway login first.";

/// Open the Railway dashboard TUI
#[derive(Parser)]
pub struct Args {
    /// Optional project ID to open directly
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Optional environment name or ID to open directly
    #[clap(short, long)]
    environment: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    crate::interact_or!("`railway dash` requires a terminal");

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs).map_err(map_dashboard_client_auth_error)?;
    let auth_mode = ensure_dashboard_login(&configs, &client).await?;

    crate::controllers::dash_tui::run(DashTuiParams {
        project: args.project,
        environment: args.environment,
        auth_mode,
    })
    .await
}

async fn ensure_dashboard_login(
    configs: &Configs,
    client: &reqwest::Client,
) -> Result<DashboardAuthMode> {
    match validate_workspace_auth(configs, client).await {
        Ok(()) => Ok(DashboardAuthMode::Workspace),
        Err(error) if should_try_linked_project_auth(&error) => {
            resolve_linked_project_auth(configs, client).await
        }
        Err(error) => Err(error.into()),
    }
}

async fn validate_workspace_auth(
    configs: &Configs,
    client: &Client,
) -> std::result::Result<(), RailwayError> {
    let _ = get_user(client, configs).await?;
    let _ = post_graphql::<queries::UserProjects, _>(
        client,
        configs.get_backboard(),
        queries::user_projects::Variables {},
    )
    .await?;

    Ok(())
}

async fn resolve_linked_project_auth(
    configs: &Configs,
    client: &Client,
) -> Result<DashboardAuthMode> {
    let linked_project = configs
        .get_linked_project()
        .await
        .map_err(map_linked_project_auth_error)?;

    let environment_input = linked_project
        .environment_name
        .clone()
        .or_else(|| linked_project.environment.clone())
        .ok_or_else(dashboard_login_error)?;

    let project = get_project(client, configs, linked_project.project.clone())
        .await
        .map_err(map_linked_project_railway_error)?;

    if project.deleted_at.is_some() {
        return Err(dashboard_login_error());
    }

    let environment = get_matched_environment(&project, environment_input)
        .map_err(map_linked_project_auth_error)?;

    if environment.deleted_at.is_some() {
        return Err(dashboard_login_error());
    }

    Ok(DashboardAuthMode::LinkedProject {
        project_id: project.id,
        environment_id: environment.id,
    })
}

fn dashboard_login_error() -> anyhow::Error {
    anyhow!(DASHBOARD_LOGIN_ERROR)
}

fn map_dashboard_client_auth_error(error: RailwayError) -> anyhow::Error {
    if should_map_auth_error_to_login(&error) {
        dashboard_login_error()
    } else {
        error.into()
    }
}

fn map_linked_project_railway_error(error: RailwayError) -> anyhow::Error {
    if should_map_linked_project_railway_error_to_login(&error) {
        dashboard_login_error()
    } else {
        error.into()
    }
}

fn map_linked_project_auth_error(error: anyhow::Error) -> anyhow::Error {
    if should_map_linked_project_anyhow_error_to_login(&error) {
        dashboard_login_error()
    } else {
        error
    }
}

fn should_try_linked_project_auth(error: &RailwayError) -> bool {
    should_map_auth_error_to_login(error)
}

fn should_map_auth_error_to_login(error: &RailwayError) -> bool {
    matches!(
        error,
        RailwayError::Unauthorized
            | RailwayError::UnauthorizedLogin
            | RailwayError::UnauthorizedToken(_)
            | RailwayError::InvalidRailwayToken(_)
    )
}

fn should_map_linked_project_railway_error_to_login(error: &RailwayError) -> bool {
    should_map_auth_error_to_login(error)
        || matches!(
            error,
            RailwayError::NoLinkedProject
                | RailwayError::ProjectNotFound
                | RailwayError::ProjectDeleted
                | RailwayError::EnvironmentDeleted
                | RailwayError::EnvironmentNotFound(_)
                | RailwayError::EnvironmentRestricted(_)
        )
}

fn should_map_linked_project_anyhow_error_to_login(error: &anyhow::Error) -> bool {
    if let Some(error) = error.downcast_ref::<RailwayError>() {
        return should_map_linked_project_railway_error_to_login(error);
    }

    let message = error.to_string();
    message.contains("No environment specified")
        || message.contains("RAILWAY_ENVIRONMENT_ID cannot be set without RAILWAY_PROJECT_ID")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_login_error_text_is_stable() {
        assert_eq!(dashboard_login_error().to_string(), DASHBOARD_LOGIN_ERROR);
    }

    #[test]
    fn unauthorized_workspace_errors_fall_back_to_linked_project_mode() {
        assert!(should_try_linked_project_auth(&RailwayError::Unauthorized));
        assert!(should_try_linked_project_auth(
            &RailwayError::UnauthorizedLogin
        ));
        assert!(should_try_linked_project_auth(
            &RailwayError::UnauthorizedToken("RAILWAY_TOKEN".to_string())
        ));
        assert!(should_try_linked_project_auth(
            &RailwayError::InvalidRailwayToken("RAILWAY_TOKEN".to_string())
        ));
    }

    #[test]
    fn non_auth_workspace_errors_do_not_fall_back_to_linked_project_mode() {
        assert!(!should_try_linked_project_auth(&RailwayError::Ratelimited));
        assert!(!should_try_linked_project_auth(
            &RailwayError::GraphQLError("boom".to_string())
        ));
    }

    #[test]
    fn linked_project_context_failures_map_to_login_error() {
        assert!(should_map_linked_project_anyhow_error_to_login(
            &RailwayError::NoLinkedProject.into()
        ));
        assert!(should_map_linked_project_anyhow_error_to_login(
            &RailwayError::ProjectDeleted.into()
        ));
        assert!(should_map_linked_project_anyhow_error_to_login(&anyhow!(
            "No environment specified. Set RAILWAY_ENVIRONMENT_ID"
        )));
    }
}
