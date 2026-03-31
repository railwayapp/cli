use super::{List as Args, *};
use crate::{
    Configs, GQLClient,
    client::post_graphql,
    commands::queries::{self, environments},
};
use anyhow::Result;
use chrono_humanize::HumanTime;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentListOutput {
    environments: Vec<EnvironmentOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentOutput {
    id: String,
    name: String,
    is_ephemeral: bool,
    is_linked: bool,
    restricted: bool,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    unmerged_changes_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_environment: Option<SourceEnvironmentOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<EnvironmentMetaOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceEnvironmentOutput {
    id: String,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentMetaOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_branch: Option<String>,
}

const PAGE_SIZE: i64 = 500;

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let linked_project = configs.get_linked_project().await?;
    let linked_env_id = linked_project.environment.as_deref();

    let is_ephemeral = if args.ephemeral {
        Some(true)
    } else if args.no_ephemeral {
        Some(false)
    } else {
        None
    };

    let mut all_edges = Vec::new();
    let mut after: Option<String> = None;

    loop {
        let vars = environments::Variables {
            project_id: linked_project.project.clone(),
            is_ephemeral,
            first: Some(PAGE_SIZE),
            after: after.take(),
        };

        let response =
            post_graphql::<queries::Environments, _>(&client, configs.get_backboard(), vars)
                .await?;

        let has_next_page = response.environments.page_info.has_next_page;
        after = response.environments.page_info.end_cursor;

        all_edges.extend(response.environments.edges);

        if !has_next_page {
            break;
        }
    }

    let mut persistent = Vec::new();
    let mut ephemeral = Vec::new();

    for edge in &all_edges {
        if edge.node.is_ephemeral {
            ephemeral.push(edge);
        } else {
            persistent.push(edge);
        }
    }

    persistent.sort_by(|a, b| {
        a.node
            .created_at
            .cmp(&b.node.created_at)
            .then_with(|| a.node.name.cmp(&b.node.name))
    });
    ephemeral.sort_by(|a, b| b.node.created_at.cmp(&a.node.created_at));

    if args.json {
        let is_linked = |id: &str| Some(id) == linked_env_id;

        let output = EnvironmentListOutput {
            environments: persistent
                .iter()
                .chain(ephemeral.iter())
                .map(|edge| {
                    let node = &edge.node;
                    let meta = node.meta.as_ref().and_then(|m| {
                        if m.pr_number.is_some() || m.branch.is_some() {
                            Some(EnvironmentMetaOutput {
                                pr_number: m.pr_number,
                                pr_title: m.pr_title.clone(),
                                pr_repo: m.pr_repo.clone(),
                                branch: m.branch.clone(),
                                base_branch: m.base_branch.clone(),
                            })
                        } else {
                            None
                        }
                    });

                    EnvironmentOutput {
                        id: node.id.clone(),
                        name: node.name.clone(),
                        is_ephemeral: node.is_ephemeral,
                        is_linked: is_linked(&node.id),
                        restricted: !node.can_access,
                        created_at: node.created_at.to_rfc3339(),
                        updated_at: node.updated_at.to_rfc3339(),
                        unmerged_changes_count: node.unmerged_changes_count,
                        source_environment: node.source_environment.as_ref().map(|s| {
                            SourceEnvironmentOutput {
                                id: s.id.clone(),
                                name: s.name.clone(),
                            }
                        }),
                        meta,
                    }
                })
                .collect(),
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if persistent.is_empty() && ephemeral.is_empty() {
        let label = match (args.ephemeral, args.no_ephemeral) {
            (true, _) => "ephemeral environments",
            (_, true) => "persistent environments",
            _ => "environments",
        };
        println!("No {label} found");
    } else {
        println!();
        if !persistent.is_empty() {
            println!("{}", "Environments".bold());
            println!();
            for edge in &persistent {
                print_environment(&edge.node, linked_env_id);
            }
        }

        if !ephemeral.is_empty() {
            if !persistent.is_empty() {
                println!();
                println!("---");
                println!();
            }
            println!("{}", "PR Environments".bold());
            println!();
            for edge in &ephemeral {
                print_environment(&edge.node, linked_env_id);
            }
        }
        println!();
    }

    Ok(())
}

fn print_environment(
    node: &environments::EnvironmentsEnvironmentsEdgesNode,
    linked_env_id: Option<&str>,
) {
    let is_linked = Some(node.id.as_str()) == linked_env_id;

    if !node.can_access {
        if is_linked {
            println!(
                "{} {} {}",
                node.name.dimmed(),
                "(linked)".green(),
                "(restricted)".dimmed()
            );
        } else {
            println!("{} {}", node.name.dimmed(), "(restricted)".dimmed());
        }
        return;
    }

    if is_linked {
        println!("{} {}", node.name, "(linked)".green());
    } else {
        println!("{}", node.name);
    }

    let mut details: Vec<String> = Vec::new();

    if let Some(source) = &node.source_environment {
        details.push(format!("forked from {}", source.name).dimmed().to_string());
    }

    if let Some(meta) = &node.meta {
        if let Some(pr_number) = meta.pr_number {
            let title_part = match meta.pr_title.as_deref().filter(|t| !t.is_empty()) {
                Some(title) => format!(": {title}"),
                None => String::new(),
            };
            let branch_info = match (&meta.branch, &meta.base_branch) {
                (Some(b), Some(base)) => format!(" ({b} <- {base})"),
                (Some(b), None) => format!(" ({b})"),
                _ => String::new(),
            };
            details.push(
                format!("PR #{pr_number}{title_part}{branch_info}")
                    .dimmed()
                    .to_string(),
            );
        } else if let Some(branch) = &meta.branch {
            let base_part = match &meta.base_branch {
                Some(base) => format!(" <- {base}"),
                None => String::new(),
            };
            details.push(format!("branch: {branch}{base_part}").dimmed().to_string());
        }
    }

    if let Some(count) = node.unmerged_changes_count.filter(|&c| c > 0) {
        details.push(
            format!(
                "{} unmerged {}",
                count,
                if count == 1 { "change" } else { "changes" }
            )
            .yellow()
            .to_string(),
        );
    }

    if node.updated_at != node.created_at {
        let human_time = HumanTime::from(node.updated_at);
        details.push(format!("updated {human_time}").dimmed().to_string());
    }

    let last = details.len().saturating_sub(1);
    for (i, detail) in details.iter().enumerate() {
        let connector = if i < last { "├" } else { "└" };
        println!("  {} {}", connector.dimmed(), detail);
    }
}
