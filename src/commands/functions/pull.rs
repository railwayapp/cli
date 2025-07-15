use crate::queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges};

use super::*;

pub async fn pull(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
    args: Pull,
) -> Result<()> {
    let (id, path) = common::get_function_from_path(args.path)?;
    let service = common::find_service(&project, environment, &id)
        .ok_or_else(|| anyhow::anyhow!("Couldn't find service"))?;

    println!(
        "Pulling from function {}",
        service.service_name.blue().bold()
    );

    let decoded_content = common::extract_function_content(&service)?;
    let current_content = std::fs::read_to_string(&path)?;
    let diff_stats = common::calculate_diff(&current_content, &decoded_content);

    std::fs::write(&path, &decoded_content)?;
    println!("Function updated {diff_stats}");

    Ok(())
}
