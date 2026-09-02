use super::*;
use crate::workspace::{
    ProjectWithWorkspace, project_favorites_by_workspace, workspaces_with_client,
};

/// Marks a favorited project in the human listing. Favorites also sort to the
/// top of their workspace, so the glyph is a confirmation of the grouping
/// rather than the only signal — which keeps the listing readable where the
/// star cannot render.
const FAVORITE_MARKER: &str = "★ ";

/// List all projects in your Railway account
#[derive(Parser)]
pub struct Args {
    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let linked_project = configs.get_linked_project().await.ok();

    let client = GQLClient::new_authorized(&configs)?;
    let workspaces = workspaces_with_client(&client, &configs).await?;

    // Favorites are fetched per workspace and joined here rather than being
    // selected alongside the projects: `projectFavorites` hangs off `Query`,
    // not off a workspace. A caller the field refuses just gets empty lists,
    // and the listing below renders exactly as it did before favorites.
    let favorites = project_favorites_by_workspace(
        &client,
        &configs,
        workspaces.iter().map(|w| w.id().to_string()).collect(),
    )
    .await;

    let mut all_projects: Vec<ProjectWithWorkspace> = Vec::new();

    for workspace in workspaces {
        let workspace_favorites = favorites
            .get(workspace.id())
            .map(Vec::as_slice)
            .unwrap_or_default();

        if !args.json {
            println!();
            println!("{}", workspace.name().bold());

            let ranked = workspace.projects_ranked(workspace_favorites);
            // Only reserve the marker's width once this workspace actually has
            // a favorite to show. With none — the degraded case for the tokens
            // the favorites field refuses, and for an account that has starred
            // nothing — every line stays byte-for-byte what it was before
            // favorites existed.
            let reserve_marker_width = ranked.iter().any(|(_, is_favorite)| *is_favorite);

            for (project, is_favorite) in ranked {
                let project_name =
                    if Some(project.id()) == linked_project.as_ref().map(|p| p.project.as_str()) {
                        project.name().purple().bold()
                    } else {
                        project.name().white()
                    };
                if is_favorite {
                    println!("  {}{project_name}", FAVORITE_MARKER.yellow());
                } else if reserve_marker_width {
                    let pad = " ".repeat(FAVORITE_MARKER.chars().count());
                    println!("  {pad}{project_name}");
                } else {
                    println!("  {project_name}");
                }
            }
        }

        all_projects.extend(workspace.projects_with_workspace(workspace_favorites));
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&all_projects)?);
    }
    Ok(())
}
