use std::fmt::Display;

use crate::{
    commands::functions::common::link_function,
    queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges},
    util::prompt::{fake_select, prompt_path, prompt_select},
};
use anyhow::bail;
use is_terminal::IsTerminal;

use super::*;

pub async fn link(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
    args: Link,
) -> Result<()> {
    let terminal = std::io::stdout().is_terminal();

    let functions = common::get_functions_in_environment(&project, environment);

    let function = if let Some(function) = args.function {
        let f = functions.iter().find(|f| {
            (f.node.service_id.to_lowercase() == function.to_lowercase())
                || (f.node.service_name.to_lowercase() == function.to_lowercase())
        });
        if let Some(function) = f {
            fake_select("Select a function", &function.node.service_name);
            function.node.clone()
        } else {
            bail!(RailwayError::ServiceNotFound(function))
        }
    } else if terminal {
        prompt_select(
            "Select a function",
            functions.iter().map(|f| f.node.clone()).collect(),
        )?
    } else {
        bail!("Function must be provided when not running in a terminal")
    };
    let path = if let Some(path) = args.path {
        fake_select(
            "Enter the path of the function",
            &path.display().to_string(),
        );
        path
    } else if terminal {
        prompt_path("Enter the path of the function")?
    } else {
        bail!("Path must be provided when not running in a terminal");
    };
    if !path.exists() {
        println!("Provided path doesn't exist, creating file");
        std::fs::write(&path, "")?;
    }
    link_function(&path, &function.service_id)?;
    let local = common::get_start_cmd(&path)?;
    let remote = function.start_command.unwrap_or_default();
    println!(
        "Linked function {} to the local file {}",
        function.service_name.blue(),
        path.display().to_string().blue()
    );
    if local != remote {
        println!(
            "The local function differs from the linked function. Run `railway functions pull -p {}` to update your local copy.",
            path.display()
        )
    }
    Ok(())
}

impl Display for queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdgesNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.service_name)
    }
}
