use crate::{
    queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges},
    util::{
        progress::{create_spinner, success_spinner},
        prompt::{fake_select, prompt_confirm_with_default, prompt_select, prompt_text},
    },
};
use anyhow::bail;
use is_terminal::IsTerminal;

use super::*;

pub async fn delete(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
    args: Delete,
) -> Result<()> {
    let terminal = std::io::stdout().is_terminal();
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let services = common::get_functions_in_environment(&project, environment);
    let function = select_function_to_delete(&args, services.as_slice(), terminal)?;

    if !confirm_deletion(&args, terminal)? {
        return Ok(());
    }

    validate_two_factor_if_enabled(&client, &configs).await?;
    delete_function_service(&client, &mut configs, function, environment).await?;

    Ok(())
}

fn select_function_to_delete<'a>(
    args: &Delete,
    services: &'a [&queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges],
    terminal: bool,
) -> Result<&'a queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges> {
    if let Some(fun) = &args.function {
        find_function_by_identifier(services, fun)
    } else if terminal {
        prompt_select("Select a function to delete", services.to_vec())
    } else {
        bail!("Function must be provided when not running in terminal")
    }
}

fn find_function_by_identifier<'a>(
    services: &'a [&queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges],
    identifier: &str,
) -> Result<&'a queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges> {
    let found = services.iter().find(|f| {
        (f.node.id.to_lowercase() == identifier.to_lowercase())
            || (f.node.service_name.to_lowercase() == identifier.to_lowercase())
    });

    match found {
        Some(function) => {
            fake_select("Select a function to delete", &function.node.service_name);
            Ok(function)
        }
        None => bail!("Service {} not found", identifier),
    }
}

fn confirm_deletion(args: &Delete, terminal: bool) -> Result<bool> {
    let yes = args.yes.unwrap_or(false);

    if yes {
        fake_select("Are you sure you want to delete this function?", "Yes");
        Ok(true)
    } else if args.yes.is_some() && !yes {
        fake_select("Are you sure you want to delete this function?", "No");
        Ok(false)
    } else if terminal {
        prompt_confirm_with_default("Are you sure you want to delete this function?", false)
    } else {
        bail!(
            "The skip confirmation flag (-y,--yes) must be provided when not running in a terminal"
        )
    }
}

async fn validate_two_factor_if_enabled(client: &reqwest::Client, configs: &Configs) -> Result<()> {
    let is_two_factor_enabled = check_two_factor_status(client, configs).await?;

    if is_two_factor_enabled {
        validate_two_factor_code(client, configs).await?;
    }

    Ok(())
}

async fn check_two_factor_status(client: &reqwest::Client, configs: &Configs) -> Result<bool> {
    let vars = queries::two_factor_info::Variables {};
    let info = post_graphql::<queries::TwoFactorInfo, _>(client, configs.get_backboard(), vars)
        .await?
        .two_factor_info;

    Ok(info.is_verified)
}

async fn validate_two_factor_code(client: &reqwest::Client, configs: &Configs) -> Result<()> {
    let token = prompt_text("Enter your 2FA code")?;
    let vars = mutations::validate_two_factor::Variables { token };

    let valid =
        post_graphql::<mutations::ValidateTwoFactor, _>(client, configs.get_backboard(), vars)
            .await?
            .two_factor_info_validate;

    if !valid {
        return Err(RailwayError::InvalidTwoFactorCode.into());
    }

    Ok(())
}

async fn delete_function_service(
    client: &reqwest::Client,
    configs: &mut Configs,
    function: &queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges,
    environment: &ProjectProjectEnvironmentsEdges,
) -> Result<()> {
    let mut spinner = create_spinner("Deleting function".into());

    post_graphql::<mutations::ServiceDelete, _>(
        client,
        configs.get_backboard(),
        mutations::service_delete::Variables {
            service_id: function.node.service_id.clone(),
            environment_id: environment.node.id.clone(),
        },
    )
    .await?;
    configs.unlink_function(function.node.service_id.clone())?;
    configs.write()?;
    success_spinner(&mut spinner, "Function deleted".into());
    Ok(())
}
