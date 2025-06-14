use super::*;
use std::path::Path;

use crate::queries::project::{
    ProjectProject, ProjectProjectEnvironmentsEdges,
    ProjectProjectServicesEdgesNodeServiceInstancesEdges,
};

pub fn get_functions_in_environment<'a>(
    project: &'a ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
) -> Vec<&'a ProjectProjectServicesEdgesNodeServiceInstancesEdges> {
    project
        .services
        .edges
        .iter()
        .filter_map(|service| {
            service
                .node
                .service_instances
                .edges
                .iter()
                .find(|instance| instance.node.environment_id == environment.node.id)
        })
        .filter(|service_instance| is_function_service(service_instance))
        .collect()
}

pub fn link_function(path: &Path, id: &str) -> Result<()> {
    let mut c = Configs::new()?;
    c.link_function(path.to_path_buf(), id.to_owned())?;
    c.write()?;
    Ok(())
}

fn is_function_service(
    service_instance: &ProjectProjectServicesEdgesNodeServiceInstancesEdges,
) -> bool {
    service_instance.node.source.clone().is_some_and(|source| {
        source
            .image
            .unwrap_or_default()
            .starts_with("ghcr.io/railwayapp/function")
    })
}
