use anyhow::{anyhow, bail};
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

#[derive(Debug)]
enum LinkedProjectAuthPreflightError {
    MissingProjectScopedContext,
    Other(anyhow::Error),
}

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
    let auth_mode = ensure_dashboard_login(&configs, &client, &args).await?;

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
    args: &Args,
) -> Result<DashboardAuthMode> {
    match validate_workspace_auth(configs, client).await {
        Ok(()) => Ok(DashboardAuthMode::Workspace),
        Err(workspace_error) if should_try_linked_project_auth(&workspace_error) => {
            match resolve_linked_project_auth(configs, client, args).await {
                Ok(auth_mode) => Ok(auth_mode),
                Err(linked_error) => Err(select_dashboard_auth_preflight_error(
                    &workspace_error,
                    linked_error,
                    Configs::get_railway_token().is_some(),
                )),
            }
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
    args: &Args,
) -> std::result::Result<DashboardAuthMode, LinkedProjectAuthPreflightError> {
    if has_incomplete_env_var_project_scope() {
        return Err(LinkedProjectAuthPreflightError::MissingProjectScopedContext);
    }

    let linked_project = configs
        .get_linked_project()
        .await
        .map_err(classify_linked_project_lookup_error)?;

    let environment_input = linked_project
        .environment_name
        .clone()
        .or_else(|| linked_project.environment.clone())
        .ok_or(LinkedProjectAuthPreflightError::MissingProjectScopedContext)?;

    let project = get_project(client, configs, linked_project.project.clone())
        .await
        .map_err(LinkedProjectAuthPreflightError::from)?;

    if project.deleted_at.is_some() {
        return Err(LinkedProjectAuthPreflightError::from(
            RailwayError::ProjectDeleted,
        ));
    }

    let environment = get_matched_environment(&project, environment_input)
        .map_err(LinkedProjectAuthPreflightError::from)?;

    validate_requested_linked_project_target(args, &project.id, &environment.id, &environment.name)
        .map_err(LinkedProjectAuthPreflightError::from)?;

    if environment.deleted_at.is_some() {
        return Err(LinkedProjectAuthPreflightError::from(
            RailwayError::EnvironmentDeleted,
        ));
    }

    Ok(DashboardAuthMode::LinkedProject {
        project_id: project.id,
        environment_id: environment.id,
    })
}

fn validate_requested_linked_project_target(
    args: &Args,
    validated_project_id: &str,
    validated_environment_id: &str,
    validated_environment_name: &str,
) -> Result<()> {
    if let Some(requested_project_id) = args.project.as_deref()
        && requested_project_id != validated_project_id
    {
        bail!(
            "`--project {requested_project_id}` does not match the authenticated project scope `{validated_project_id}`"
        );
    }

    if let Some(requested_environment) = args.environment.as_deref()
        && requested_environment != validated_environment_id
        && requested_environment != validated_environment_name
    {
        bail!(
            "`--environment {requested_environment}` does not match the authenticated environment scope `{validated_environment_name}` ({validated_environment_id})"
        );
    }

    Ok(())
}

fn dashboard_login_error() -> anyhow::Error {
    anyhow!(DASHBOARD_LOGIN_ERROR)
}

fn project_token_requires_link_error() -> anyhow::Error {
    anyhow!(
        "`railway dash` is running with RAILWAY_TOKEN, which cannot open the workspace project picker. Link a project and environment first with `railway link` and `railway environment`, or unset RAILWAY_TOKEN and use `railway login`."
    )
}

fn select_dashboard_auth_preflight_error(
    workspace_error: &RailwayError,
    linked_error: LinkedProjectAuthPreflightError,
    is_project_token_auth: bool,
) -> anyhow::Error {
    match linked_error {
        LinkedProjectAuthPreflightError::MissingProjectScopedContext => {
            if is_project_token_auth {
                project_token_requires_link_error()
            } else {
                anyhow!(workspace_error.to_string())
            }
        }
        LinkedProjectAuthPreflightError::Other(error) => error,
    }
}

fn map_dashboard_client_auth_error(error: RailwayError) -> anyhow::Error {
    match error {
        RailwayError::Unauthorized => dashboard_login_error(),
        other => other.into(),
    }
}

fn should_try_linked_project_auth(error: &RailwayError) -> bool {
    should_map_auth_error_to_fallback(error)
}

fn classify_linked_project_lookup_error(error: anyhow::Error) -> LinkedProjectAuthPreflightError {
    if error
        .downcast_ref::<RailwayError>()
        .is_some_and(|error| matches!(error, RailwayError::NoLinkedProject))
    {
        LinkedProjectAuthPreflightError::MissingProjectScopedContext
    } else {
        LinkedProjectAuthPreflightError::Other(error)
    }
}

fn has_incomplete_env_var_project_scope() -> bool {
    has_incomplete_env_var_project_scope_from(
        Configs::get_railway_project_id(),
        Configs::get_railway_environment_id(),
    )
}

fn has_incomplete_env_var_project_scope_from(
    project_id: Option<String>,
    environment_id: Option<String>,
) -> bool {
    project_id.is_none() && environment_id.is_some()
}

fn should_map_auth_error_to_fallback(error: &RailwayError) -> bool {
    matches!(
        error,
        RailwayError::Unauthorized
            | RailwayError::UnauthorizedLogin
            | RailwayError::UnauthorizedToken(_)
            | RailwayError::InvalidRailwayToken(_)
    )
}

impl From<anyhow::Error> for LinkedProjectAuthPreflightError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<RailwayError> for LinkedProjectAuthPreflightError {
    fn from(error: RailwayError) -> Self {
        Self::Other(error.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_login_error_text_is_stable() {
        assert_eq!(dashboard_login_error().to_string(), DASHBOARD_LOGIN_ERROR);
    }

    #[test]
    fn project_token_requires_link_error_is_explicit() {
        assert!(
            project_token_requires_link_error()
                .to_string()
                .contains("RAILWAY_TOKEN")
        );
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
    fn only_missing_auth_maps_to_generic_login_message() {
        assert_eq!(
            map_dashboard_client_auth_error(RailwayError::Unauthorized).to_string(),
            DASHBOARD_LOGIN_ERROR
        );
        assert!(
            map_dashboard_client_auth_error(RailwayError::UnauthorizedLogin)
                .to_string()
                .contains("railway login")
        );
        assert!(
            map_dashboard_client_auth_error(RailwayError::InvalidRailwayToken(
                "RAILWAY_TOKEN".to_string()
            ))
            .to_string()
            .contains("Invalid RAILWAY_TOKEN")
        );
    }

    #[test]
    fn missing_link_context_uses_workspace_auth_error_for_login_sessions() {
        let error = select_dashboard_auth_preflight_error(
            &RailwayError::UnauthorizedLogin,
            LinkedProjectAuthPreflightError::MissingProjectScopedContext,
            false,
        );

        assert_eq!(
            error.to_string(),
            RailwayError::UnauthorizedLogin.to_string()
        );
    }

    #[test]
    fn missing_link_context_is_explicit_for_project_token_auth() {
        let error = select_dashboard_auth_preflight_error(
            &RailwayError::UnauthorizedToken("RAILWAY_TOKEN".to_string()),
            LinkedProjectAuthPreflightError::MissingProjectScopedContext,
            true,
        );

        assert!(error.to_string().contains("RAILWAY_TOKEN"));
        assert!(error.to_string().contains("railway link"));
    }

    #[test]
    fn resource_errors_are_not_collapsed_into_login_message() {
        let error = select_dashboard_auth_preflight_error(
            &RailwayError::UnauthorizedToken("RAILWAY_TOKEN".to_string()),
            LinkedProjectAuthPreflightError::from(RailwayError::EnvironmentRestricted(
                "prod".to_string(),
            )),
            true,
        );

        assert_eq!(
            error.to_string(),
            RailwayError::EnvironmentRestricted("prod".to_string()).to_string()
        );
    }

    #[test]
    fn linked_project_lookup_classification_is_precise() {
        assert!(matches!(
            classify_linked_project_lookup_error(RailwayError::NoLinkedProject.into()),
            LinkedProjectAuthPreflightError::MissingProjectScopedContext
        ));
        assert!(matches!(
            classify_linked_project_lookup_error(RailwayError::ProjectDeleted.into()),
            LinkedProjectAuthPreflightError::Other(_)
        ));
    }

    #[test]
    fn incomplete_env_var_project_scope_is_missing_context() {
        assert!(has_incomplete_env_var_project_scope_from(
            None,
            Some("env_123".to_string())
        ));
        assert!(!has_incomplete_env_var_project_scope_from(
            Some("proj_123".to_string()),
            Some("env_123".to_string())
        ));
        assert!(!has_incomplete_env_var_project_scope_from(None, None));
    }

    #[test]
    fn linked_project_target_validation_accepts_matching_inputs() {
        let args = Args {
            project: Some("proj_123".to_string()),
            environment: Some("production".to_string()),
        };

        validate_requested_linked_project_target(&args, "proj_123", "env_123", "production")
            .unwrap();

        let args = Args {
            project: Some("proj_123".to_string()),
            environment: Some("env_123".to_string()),
        };

        validate_requested_linked_project_target(&args, "proj_123", "env_123", "production")
            .unwrap();
    }

    #[test]
    fn linked_project_target_validation_rejects_project_mismatch() {
        let args = Args {
            project: Some("proj_requested".to_string()),
            environment: None,
        };

        let error = validate_requested_linked_project_target(
            &args,
            "proj_validated",
            "env_123",
            "production",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("does not match the authenticated project scope")
        );
    }

    #[test]
    fn linked_project_target_validation_rejects_environment_mismatch() {
        let args = Args {
            project: None,
            environment: Some("staging".to_string()),
        };

        let error =
            validate_requested_linked_project_target(&args, "proj_123", "env_123", "production")
                .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("does not match the authenticated environment scope")
        );
    }
}
