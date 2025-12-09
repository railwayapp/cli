use crate::{
    commands::functions::common::unlink_function,
    queries::project::ProjectProjectEnvironmentsEdges,
    util::{
        progress::{create_spinner, success_spinner},
        prompt::{fake_select, prompt_select, prompt_text},
    },
};
use anyhow::bail;
use is_terminal::IsTerminal;

use super::*;

pub async fn delete(environment: &ProjectProjectEnvironmentsEdges, args: Delete) -> Result<()> {
    let terminal = std::io::stdout().is_terminal();
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let services = common::get_functions_in_environment(environment);
    let function = select_function_to_delete(&args, services.as_slice(), terminal)?;

    if !common::confirm(
        args.yes,
        terminal,
        "Are you sure you want to delete this function?",
    )? {
        return Ok(());
    }

    validate_two_factor_if_enabled(&client, &configs).await?;
    delete_function_service(&client, &mut configs, function, environment).await?;

    Ok(())
}

fn select_function_to_delete<'a>(
    args: &Delete,
    services: &'a [&queries::project::ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges],
    terminal: bool,
) -> Result<&'a queries::project::ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges> {
    if let Some(fun) = &args.function {
        find_function_by_identifier(services, fun)
    } else if terminal {
        prompt_select("Select a function to delete", services.to_vec())
    } else {
        bail!("Function must be provided when not running in terminal")
    }
}

fn find_function_by_identifier<'a>(
    services: &'a [&queries::project::ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges],
    identifier: &str,
) -> Result<&'a queries::project::ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges> {
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
    function: &queries::project::ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges,
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
    unlink_function(&function.node.service_id)?;
    success_spinner(&mut spinner, "Function deleted".into());
    Ok(())
}
