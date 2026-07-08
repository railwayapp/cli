use std::{cmp::Ordering, fmt::Display};

use anyhow::bail;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};

use crate::{
    commands::output::fields::print_field,
    errors::RailwayError,
    util::{
        progress::create_spinner_if,
        prompt::{prompt_confirm_with_default, prompt_select},
    },
    workspace::{Project, Workspace, workspaces_with_client},
};

use super::*;

const FIELD_LABEL_WIDTH: usize = 18;
const PROJECT_NAME_MIN_WIDTH: usize = 30;
const PROJECT_NAME_MAX_WIDTH: usize = 64;
const CURRENT_COST_COLUMN_WIDTH: usize = 12;
const SERVICE_NAME_WIDTH: usize = 18;

const MINUTES_IN_MONTH: f64 = 43_200.0;
const PRICE_MINUTELY_MEM_GB: f64 = 10.0 / MINUTES_IN_MONTH;
const PRICE_MINUTELY_VCPU: f64 = 20.0 / MINUTES_IN_MONTH;
const PRICE_EGRESS_GB: f64 = 0.00005 * 1_000.0;
const PRICE_MINUTELY_DISK_GB: f64 = 0.15 / MINUTES_IN_MONTH;
const PRICE_MINUTELY_BACKUP_GB: f64 = PRICE_MINUTELY_DISK_GB;
const MIN_SOFT_USAGE_LIMIT_DOLLARS: u32 = 5;
const MIN_HARD_USAGE_LIMIT_DOLLARS: u32 = 10;
const MAX_USAGE_LIMIT_DOLLARS: u32 = 500_000;

const USAGE_MEASUREMENTS: &[&str] = &[
    "MEMORY_USAGE_GB",
    "CPU_USAGE",
    "NETWORK_TX_GB",
    "DISK_USAGE_GB",
    "BACKUP_USAGE_GB",
];

const WORKSPACE_USAGE_CONTEXT_QUERY: &str = r#"
query WorkspaceUsageContext($workspaceId: String!) {
  workspace(workspaceId: $workspaceId) {
    id
    name
    customer {
      id
      currentUsage
      billingPeriod {
        start
        end
      }
      usageLimit {
        softLimit
        hardLimit
        isOverLimit
      }
    }
  }
}
"#;

const WORKSPACE_USAGE_QUERY: &str = r#"
query WorkspaceUsage(
  $workspaceId: String!
  $measurements: [MetricMeasurement!]!
  $startDate: DateTime!
  $endDate: DateTime!
) {
  usage(
    workspaceId: $workspaceId
    measurements: $measurements
    groupBy: [PROJECT_ID]
    startDate: $startDate
    endDate: $endDate
    includeDeleted: true
  ) {
    measurement
    value
    tags {
      projectId
      serviceId
    }
  }
  projects(first: 5000, includeDeleted: true, workspaceId: $workspaceId) {
    edges {
      node {
        id
        name
        deletedAt
      }
    }
  }
}
"#;

const WORKSPACE_ESTIMATED_USAGE_QUERY: &str = r#"
query WorkspaceEstimatedUsage(
  $workspaceId: String!
  $measurements: [MetricMeasurement!]!
) {
  estimatedUsage(
    workspaceId: $workspaceId
    measurements: $measurements
    includeDeleted: true
  ) {
    measurement
    estimatedValue
  }
}
"#;

const PROJECT_USAGE_QUERY: &str = r#"
query ProjectUsage(
  $workspaceId: String!
  $measurements: [MetricMeasurement!]!
  $startDate: DateTime!
  $endDate: DateTime!
) {
  usage(
    workspaceId: $workspaceId
    measurements: $measurements
    groupBy: [PROJECT_ID, SERVICE_ID]
    startDate: $startDate
    endDate: $endDate
    includeDeleted: true
  ) {
    measurement
    value
    tags {
      projectId
      serviceId
    }
  }
  projects(first: 5000, includeDeleted: true, workspaceId: $workspaceId) {
    edges {
      node {
        id
        name
        deletedAt
        services {
          edges {
            node {
              id
              name
              deletedAt
            }
          }
        }
      }
    }
  }
}
"#;

const USAGE_LIMIT_SET_MUTATION: &str = r#"
mutation UsageLimitSet($input: UsageLimitSetInput!) {
  usageLimitSet(input: $input)
}
"#;

const USAGE_LIMIT_REMOVE_MUTATION: &str = r#"
mutation UsageLimitRemove($input: UsageLimitRemoveInput!) {
  usageLimitRemove(input: $input)
}
"#;

const AGENT_USAGE_QUERY: &str = r#"
query AgentUsage($workspaceId: String!) {
  agentUsage(workspaceId: $workspaceId) {
    totalUsedCents
    hardLimitCents
    softLimitCents
    usageRemaining
    billingPeriodEnd
  }
}
"#;

const AGENT_USAGE_LIMIT_SET_MUTATION: &str = r#"
mutation AgentUsageLimitSet($input: AgentUsageLimitSetInput!) {
  agentUsageLimitSet(input: $input)
}
"#;

/// Show workspace usage and manage usage limits
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway usage\n  railway usage --period previous --json\n  railway usage projects --limit 10\n  railway usage projects --project api --period 2026-07\n  railway usage limit status\n  railway usage limit status --target agent\n  railway usage limit set --target workspace --soft 75 --hard 125\n  railway usage limit set --target agent --soft 7.50 --hard 20 --workspace Acme\n  railway usage limit update --soft 75\n  railway usage limit remove --yes --json\n\nAutomation notes:\n  Usage is scoped to a workspace billing period. --period accepts current, previous, or YYYY-MM and applies to usage summaries and project breakdowns only.\n  usage projects prints the top 25 projects by default; --json returns all projects unless --limit is supplied."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    /// Workspace name or ID
    #[clap(long, global = true)]
    workspace: Option<String>,

    /// Billing period: current, previous, or YYYY-MM
    #[clap(long, value_parser = parse_period)]
    period: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// Show usage by project
    Projects(ProjectsArgs),

    /// Show or update usage limits
    Limit(LimitArgs),
}

#[derive(Parser)]
struct ProjectsArgs {
    /// Project name or ID for service-level usage
    #[clap(long, conflicts_with = "limit")]
    project: Option<String>,

    /// Billing period: current, previous, or YYYY-MM
    #[clap(long, value_parser = parse_period)]
    period: Option<String>,

    /// Maximum number of projects to return
    #[clap(long, value_parser = parse_limit)]
    limit: Option<usize>,
}

#[derive(Parser)]
struct LimitArgs {
    #[clap(subcommand)]
    command: LimitCommands,
}

#[derive(Parser)]
enum LimitCommands {
    /// Show usage limit status
    Status(StatusLimitArgs),

    /// Set usage limits
    Set(SetLimitArgs),

    /// Update compute usage limits
    Update(UpdateLimitArgs),

    /// Remove compute usage limits
    Remove(RemoveLimitArgs),
}

#[derive(Parser)]
struct StatusLimitArgs {
    /// Limit target to show
    #[clap(long, value_enum)]
    target: Option<LimitTarget>,
}

#[derive(Parser)]
#[clap(group(
    clap::ArgGroup::new("limit_value")
        .required(true)
        .multiple(true)
        .args(["soft", "hard"])
))]
struct SetLimitArgs {
    /// Limit target to set
    #[clap(long, value_enum)]
    target: LimitTarget,

    /// Email alert in dollars
    #[clap(long, value_parser = parse_limit_amount)]
    soft: Option<LimitAmount>,

    /// Hard limit in dollars
    #[clap(long, value_parser = parse_limit_amount)]
    hard: Option<LimitAmount>,
}

#[derive(Parser)]
#[clap(group(
    clap::ArgGroup::new("limit_value")
        .required(true)
        .multiple(true)
        .args(["soft", "hard"])
))]
struct UpdateLimitArgs {
    /// Compute email alert in whole dollars
    #[clap(long, value_parser = parse_limit_amount)]
    soft: Option<LimitAmount>,

    /// Compute hard limit in whole dollars
    #[clap(long, value_parser = parse_limit_amount)]
    hard: Option<LimitAmount>,
}

#[derive(Parser)]
struct RemoveLimitArgs {
    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,
}

pub async fn command(args: Args) -> Result<()> {
    crate::util::reporter::set_mode(args.json);

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    match args.command {
        Some(Commands::Projects(projects_args)) => {
            let period = projects_args.period.or(args.period);
            projects(
                &client,
                &configs,
                args.workspace,
                projects_args.project,
                period,
                projects_args.limit,
                args.json,
            )
            .await?
        }
        Some(Commands::Limit(limit_args)) => {
            if args.period.is_some() {
                bail!("--period is not supported for usage limit commands");
            }
            limit(&client, &configs, args.workspace, limit_args, args.json).await?
        }
        None => summary(&client, &configs, args.workspace, args.period, args.json).await?,
    }

    Ok(())
}

async fn summary(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_arg: Option<String>,
    period: Option<String>,
    json: bool,
) -> Result<()> {
    let spinner = loading_spinner(json, "Loading usage...");
    let workspace = resolve_workspace(client, configs, workspace_arg, spinner.as_ref()).await?;
    let summary =
        fetch_workspace_usage_summary(client, configs, workspace.id(), period.as_deref()).await?;
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    if json {
        print_usage_summary_json(&summary)?;
    } else {
        print_usage_summary(&summary);
    }

    Ok(())
}

async fn projects(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_arg: Option<String>,
    project_arg: Option<String>,
    period: Option<String>,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    if project_arg.is_some() && limit.is_some() {
        bail!("--limit is not supported with --project");
    }

    let spinner = loading_spinner(json, "Loading usage...");

    if let Some(project_arg) = project_arg {
        let workspace = resolve_workspace(client, configs, workspace_arg, spinner.as_ref()).await?;
        let context = fetch_workspace_usage_context(client, configs, workspace.id()).await?;
        let resolved_period =
            resolve_usage_period(&context.customer.billing_period, period.as_deref())?;
        let project_id = resolve_project_in_workspace(&workspace, &project_arg)?;
        let project_summary = fetch_project_usage_summary(
            client,
            configs,
            &context.workspace(),
            workspace.id(),
            &project_id,
            &resolved_period,
        )
        .await?;

        if let Some(spinner) = spinner {
            spinner.finish_and_clear();
        }

        if json {
            print_project_usage_json(&project_summary)?;
        } else {
            print_project_usage(&project_summary);
        }

        return Ok(());
    }

    let workspace = resolve_workspace(client, configs, workspace_arg, spinner.as_ref()).await?;
    let summary =
        fetch_workspace_usage_summary(client, configs, workspace.id(), period.as_deref()).await?;
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    if json {
        print_projects_json(&summary, limit)?;
    } else {
        print_projects(&summary, limit.unwrap_or(25));
    }

    Ok(())
}

async fn limit(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_arg: Option<String>,
    args: LimitArgs,
    json: bool,
) -> Result<()> {
    let spinner = loading_spinner(json, "Loading usage limit...");
    let workspace = resolve_workspace(client, configs, workspace_arg, spinner.as_ref()).await?;

    match args.command {
        LimitCommands::Status(status_args) => match status_args.target {
            Some(LimitTarget::Workspace) => {
                let summary =
                    fetch_workspace_usage_summary(client, configs, workspace.id(), None).await?;
                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }
                if json {
                    print_limit_json("status", &summary)?;
                } else {
                    print_limit_status(&summary);
                }
            }
            Some(LimitTarget::Agent) => {
                let agent_usage = fetch_agent_usage(client, configs, workspace.id()).await?;
                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }
                if json {
                    print_agent_limit_json("status", &workspace, &agent_usage)?;
                } else {
                    print_agent_limit_status(&workspace, &agent_usage);
                }
            }
            None => {
                let summary =
                    fetch_workspace_usage_summary(client, configs, workspace.id(), None).await?;
                let agent_usage = fetch_agent_usage(client, configs, workspace.id()).await?;
                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }
                if json {
                    print_combined_limit_json(&summary, &agent_usage)?;
                } else {
                    print_combined_limit_status(&summary, &agent_usage);
                }
            }
        },
        LimitCommands::Set(set_args) => match set_args.target {
            LimitTarget::Workspace => {
                let before =
                    fetch_workspace_usage_summary(client, configs, workspace.id(), None).await?;
                let request = usage_limit_set_request(
                    set_args.soft,
                    set_args.hard,
                    before.usage_limit.as_ref(),
                )?;
                set_usage_limit(
                    client,
                    configs,
                    &before.customer.id,
                    request.soft_limit,
                    request.hard_limit,
                )
                .await?;
                let after =
                    fetch_workspace_usage_summary(client, configs, workspace.id(), None).await?;
                let action = if before.usage_limit.is_some() {
                    "updated"
                } else {
                    "created"
                };

                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }

                if json {
                    print_limit_json(action, &after)?;
                } else {
                    print_limit_action(action, &after);
                }
            }
            LimitTarget::Agent => {
                let request = agent_usage_limit_set_request(&set_args)?;
                set_agent_usage_limit(client, configs, workspace.id(), &request).await?;
                let agent_usage = fetch_agent_usage(client, configs, workspace.id()).await?;

                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }

                if json {
                    print_agent_limit_json("updated", &workspace, &agent_usage)?;
                } else {
                    print_agent_limit_action("updated", &workspace, &agent_usage);
                }
            }
        },
        LimitCommands::Update(update_args) => {
            let before =
                fetch_workspace_usage_summary(client, configs, workspace.id(), None).await?;
            let request = usage_limit_set_request(
                update_args.soft,
                update_args.hard,
                before.usage_limit.as_ref(),
            )?;
            set_usage_limit(
                client,
                configs,
                &before.customer.id,
                request.soft_limit,
                request.hard_limit,
            )
            .await?;
            let after =
                fetch_workspace_usage_summary(client, configs, workspace.id(), None).await?;
            let action = if before.usage_limit.is_some() {
                "updated"
            } else {
                "created"
            };

            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }

            if json {
                print_limit_json(action, &after)?;
            } else {
                print_limit_action(action, &after);
            }
        }
        LimitCommands::Remove(remove_args) => {
            let before =
                fetch_workspace_usage_summary(client, configs, workspace.id(), None).await?;
            if let Some(spinner) = &spinner {
                spinner.finish_and_clear();
            }
            confirm_remove_usage_limit(remove_args.yes, &before.workspace.name)?;

            let spinner = loading_spinner(json, "Removing usage limit...");
            remove_usage_limit(client, configs, &before.customer.id).await?;
            let after =
                fetch_workspace_usage_summary(client, configs, workspace.id(), None).await?;
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }

            if json {
                print_limit_json("removed", &after)?;
            } else {
                print_limit_action("removed", &after);
            }
        }
    }

    Ok(())
}

async fn resolve_workspace(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_arg: Option<String>,
    spinner: Option<&indicatif::ProgressBar>,
) -> Result<Workspace> {
    let workspaces = workspaces_with_client(client, configs).await?;

    if workspaces.is_empty() {
        bail!("No workspaces found. Create a project at https://railway.com/new");
    }

    if let Some(input) = workspace_arg {
        return workspaces
            .iter()
            .find(|w| w.id().eq_ignore_ascii_case(&input) || w.name().eq_ignore_ascii_case(&input))
            .cloned()
            .ok_or_else(|| RailwayError::WorkspaceNotFound(input).into());
    }

    if let Ok(linked_project) = configs.get_linked_project().await {
        if let Some(workspace) = workspace_for_project(&workspaces, &linked_project.project) {
            return Ok(workspace);
        }
    }

    if workspaces.len() == 1 {
        return Ok(workspaces[0].clone());
    }

    if !std::io::stdout().is_terminal() {
        bail!("--workspace required in non-interactive mode (multiple workspaces available)");
    }

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    prompt_select("Select a workspace", workspaces)
}

fn workspace_for_project(workspaces: &[Workspace], project_id: &str) -> Option<Workspace> {
    workspaces
        .iter()
        .find(|workspace| {
            workspace
                .projects()
                .iter()
                .any(|project| project.id() == project_id)
        })
        .cloned()
}

fn summary_workspace_from_workspace(workspace: &Workspace) -> SummaryWorkspace {
    SummaryWorkspace {
        id: workspace.id().to_string(),
        name: workspace.name().to_string(),
    }
}

fn loading_spinner(json: bool, message: &str) -> Option<indicatif::ProgressBar> {
    create_spinner_if(
        !json && std::io::stdout().is_terminal(),
        message.to_string(),
    )
}

fn resolve_project_in_workspace(workspace: &Workspace, input: &str) -> Result<String> {
    let mut matches = workspace
        .projects()
        .into_iter()
        .filter(|project| matches_project(project, input))
        .map(|project| ProjectMatch {
            id: project.id().to_string(),
            name: project.name().to_string(),
        })
        .collect::<Vec<_>>();

    match matches.len() {
        0 => Ok(input.to_string()),
        1 => Ok(matches.remove(0).id),
        _ => {
            let available = matches
                .iter()
                .map(|project| format!("{} ({})", project.id, project.name))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("Ambiguous project \"{input}\". Use one of these project IDs: {available}")
        }
    }
}

fn matches_project(project: &Project, input: &str) -> bool {
    matches_id_or_name(project.id(), project.name(), input)
}

fn matches_id_or_name(id: &str, name: &str, input: &str) -> bool {
    id.eq_ignore_ascii_case(input) || name.eq_ignore_ascii_case(input)
}

async fn fetch_workspace_usage_summary(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_id: &str,
    period: Option<&str>,
) -> Result<WorkspaceUsageSummary> {
    let context = fetch_workspace_usage_context(client, configs, workspace_id).await?;
    let resolved_period = resolve_usage_period(&context.customer.billing_period, period)?;
    let usage = fetch_workspace_usage(client, configs, workspace_id, &resolved_period).await?;
    let current_item = usage_item_from_aggregated(&usage.usage);
    let metrics_usage_dollars = cost_for_usage_item(&current_item);
    let current_usage_dollars = if resolved_period.is_current_period {
        context
            .customer
            .current_usage
            .unwrap_or(metrics_usage_dollars)
    } else {
        metrics_usage_dollars
    };
    let estimated_bill_dollars = if resolved_period.is_current_period {
        let estimated = fetch_workspace_estimated_usage(client, configs, workspace_id)
            .await
            .ok()
            .map(|estimates| cost_for_usage_item(&usage_item_from_estimated(&estimates)));
        estimated
            .map(|estimated| estimated + (current_usage_dollars - metrics_usage_dollars).max(0.0))
    } else {
        None
    };
    let total_project_usage = project_usage_summaries(&usage.projects.nodes(), &usage.usage);

    Ok(WorkspaceUsageSummary {
        workspace: context.workspace(),
        customer: SummaryCustomer {
            id: context.customer.id,
        },
        billing_period: resolved_period.billing_period,
        period: resolved_period.period,
        is_current_period: resolved_period.is_current_period,
        current_usage_dollars,
        current_bill_dollars: current_usage_dollars,
        estimated_bill_dollars,
        usage_limit: if resolved_period.is_current_period {
            context.customer.usage_limit
        } else {
            None
        },
        line_items: line_items_from_usage_item(&current_item),
        projects: total_project_usage,
    })
}

async fn fetch_workspace_usage_context(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_id: &str,
) -> Result<WorkspaceUsageContext> {
    let response: WorkspaceUsageContextResponse = post_graphql_raw(
        client,
        configs.get_backboard(),
        WORKSPACE_USAGE_CONTEXT_QUERY,
        serde_json::json!({
            "workspaceId": workspace_id,
        }),
    )
    .await?;

    Ok(response.workspace)
}

async fn fetch_workspace_usage(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_id: &str,
    period: &ResolvedUsagePeriod,
) -> Result<WorkspaceUsageResponse> {
    Ok(post_graphql_raw(
        client,
        configs.get_backboard(),
        WORKSPACE_USAGE_QUERY,
        serde_json::json!({
            "workspaceId": workspace_id,
            "measurements": USAGE_MEASUREMENTS,
            "startDate": period.billing_period.start,
            "endDate": period.billing_period.end,
        }),
    )
    .await?)
}

async fn fetch_workspace_estimated_usage(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_id: &str,
) -> Result<Vec<EstimatedUsage>> {
    let response: WorkspaceEstimatedUsageResponse = post_graphql_raw(
        client,
        configs.get_backboard(),
        WORKSPACE_ESTIMATED_USAGE_QUERY,
        serde_json::json!({
            "workspaceId": workspace_id,
            "measurements": USAGE_MEASUREMENTS,
        }),
    )
    .await?;

    Ok(response.estimated_usage)
}

async fn fetch_project_usage_summary(
    client: &reqwest::Client,
    configs: &Configs,
    workspace: &SummaryWorkspace,
    workspace_id: &str,
    project_id: &str,
    period: &ResolvedUsagePeriod,
) -> Result<ProjectUsageSummary> {
    let response: ProjectUsageResponse = post_graphql_raw(
        client,
        configs.get_backboard(),
        PROJECT_USAGE_QUERY,
        serde_json::json!({
            "workspaceId": workspace_id,
            "measurements": USAGE_MEASUREMENTS,
            "startDate": period.billing_period.start,
            "endDate": period.billing_period.end,
        }),
    )
    .await?;

    let project = response
        .projects
        .nodes()
        .into_iter()
        .find(|project| project.id == project_id)
        .ok_or_else(|| anyhow::anyhow!("Project \"{project_id}\" not found in workspace"))?;
    let project_usage = response
        .usage
        .iter()
        .filter(|usage| usage.tags.project_id.as_deref() == Some(project_id))
        .cloned()
        .collect::<Vec<_>>();
    let project_services = project
        .services
        .as_ref()
        .map(ServiceConnection::nodes)
        .unwrap_or_default();
    let services = service_usage_summaries(&project_services, &project_usage);
    let current_usage_dollars = services
        .iter()
        .map(|service| service.total_dollars)
        .sum::<f64>();

    Ok(ProjectUsageSummary {
        workspace: workspace.clone(),
        project: ProjectUsageProject {
            id: project.id,
            name: project.name,
            deleted_at: project.deleted_at,
        },
        billing_period: period.billing_period.clone(),
        period: period.period.clone(),
        current_usage_dollars,
        services,
    })
}

async fn set_usage_limit(
    client: &reqwest::Client,
    configs: &Configs,
    customer_id: &str,
    soft_limit: u32,
    hard_limit: Option<u32>,
) -> Result<()> {
    let _response: serde_json::Value = post_graphql_raw(
        client,
        configs.get_backboard(),
        USAGE_LIMIT_SET_MUTATION,
        serde_json::json!({
            "input": {
                "customerId": customer_id,
                "softLimitDollars": soft_limit,
                "hardLimitDollars": hard_limit,
            }
        }),
    )
    .await?;

    Ok(())
}

async fn fetch_agent_usage(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_id: &str,
) -> Result<AgentUsageSummary> {
    let response: AgentUsageResponse = post_graphql_raw(
        client,
        configs.get_backboard(),
        AGENT_USAGE_QUERY,
        serde_json::json!({
            "workspaceId": workspace_id,
        }),
    )
    .await?;

    Ok(response.agent_usage)
}

async fn set_agent_usage_limit(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_id: &str,
    request: &AgentUsageLimitSetRequest,
) -> Result<()> {
    let _response: serde_json::Value = post_graphql_raw(
        client,
        configs.get_backboard(),
        AGENT_USAGE_LIMIT_SET_MUTATION,
        serde_json::json!({
            "input": {
                "workspaceId": workspace_id,
                "hardLimitCents": request.hard_limit_cents,
                "softLimitCents": request.soft_limit_cents,
            }
        }),
    )
    .await?;

    Ok(())
}

async fn remove_usage_limit(
    client: &reqwest::Client,
    configs: &Configs,
    customer_id: &str,
) -> Result<()> {
    let _response: serde_json::Value = post_graphql_raw(
        client,
        configs.get_backboard(),
        USAGE_LIMIT_REMOVE_MUTATION,
        serde_json::json!({
            "input": {
                "customerId": customer_id,
            }
        }),
    )
    .await?;

    Ok(())
}

fn resolve_usage_period(
    period: &BillingPeriod,
    requested: Option<&str>,
) -> Result<ResolvedUsagePeriod> {
    let current_start = parse_rfc3339_utc(&period.start)?;
    let current_end = parse_rfc3339_utc(&period.end)?;
    let requested = requested.unwrap_or("current");

    let (start, end) = match requested {
        "current" => (current_start, current_end),
        "previous" => (shift_months(current_start, -1)?, current_start),
        value => {
            let Some((year, month)) = value.split_once('-') else {
                bail!("Usage period must be current, previous, or YYYY-MM");
            };
            let year = year.parse::<i32>()?;
            let month = month.parse::<u32>()?;
            if !(1..=12).contains(&month) {
                bail!("Usage period must be current, previous, or YYYY-MM");
            }

            let current_month_index = current_start.year() * 12 + current_start.month0() as i32;
            let target_month_index = year * 12 + (month - 1) as i32;
            let offset = target_month_index - current_month_index;
            (
                shift_months(current_start, offset)?,
                shift_months(current_end, offset)?,
            )
        }
    };

    let is_current_period = start == current_start && end == current_end;
    if !is_current_period && start >= current_end {
        bail!("Cannot query future usage periods");
    }

    if !is_current_period && start < Utc::now() - chrono::Duration::days(90) {
        bail!("Usage data is only available for the last 90 days");
    }

    Ok(ResolvedUsagePeriod {
        billing_period: BillingPeriod {
            start: start.to_rfc3339(),
            end: end.to_rfc3339(),
        },
        period: requested.to_string(),
        is_current_period,
    })
}

fn shift_months(date: DateTime<Utc>, offset: i32) -> Result<DateTime<Utc>> {
    let naive = date.naive_utc();
    let month_index = naive.date().year() * 12 + naive.date().month0() as i32 + offset;
    let year = month_index.div_euclid(12);
    let month0 = month_index.rem_euclid(12);
    let month = (month0 + 1) as u32;
    let day = naive.date().day().min(days_in_month(year, month));
    let shifted = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| anyhow::anyhow!("Invalid usage period"))?
        .and_time(naive.time());

    Ok(DateTime::from_naive_utc_and_offset(shifted, Utc))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next_month_start = NaiveDate::from_ymd_opt(next_year, next_month, 1).unwrap();
    (next_month_start - chrono::Duration::days(1)).day()
}

fn parse_rfc3339_utc(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn project_usage_summaries(
    projects: &[WorkspaceUsageProjectNode],
    usage: &[AggregatedUsage],
) -> Vec<WorkspaceUsageProject> {
    let total_usage_dollars = cost_for_usage_item(&usage_item_from_aggregated(usage));
    let mut summaries = projects
        .iter()
        .map(|project| {
            let item = usage_item_from_aggregated(
                &usage
                    .iter()
                    .filter(|sample| sample.tags.project_id.as_deref() == Some(project.id.as_str()))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            let current_usage_dollars = cost_for_usage_item(&item);
            WorkspaceUsageProject {
                id: project.id.clone(),
                name: project.name.clone(),
                deleted_at: project.deleted_at.clone(),
                current_usage_dollars,
                share: share(current_usage_dollars, total_usage_dollars),
            }
        })
        .filter(|project| project.current_usage_dollars > 0.0)
        .collect::<Vec<_>>();

    sort_workspace_usage_projects(&mut summaries);
    summaries
}

fn service_usage_summaries(
    services: &[ProjectUsageServiceNode],
    usage: &[AggregatedUsage],
) -> Vec<ProjectUsageService> {
    let service_ids = usage
        .iter()
        .filter_map(|sample| sample.tags.service_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut summaries = service_ids
        .into_iter()
        .map(|service_id| {
            let item = usage_item_from_aggregated(
                &usage
                    .iter()
                    .filter(|sample| sample.tags.service_id.as_deref() == Some(service_id.as_str()))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            let service = services.iter().find(|service| service.id == service_id);
            let breakdown = cost_breakdown(&item);

            ProjectUsageService {
                id: service_id,
                name: service
                    .map(|service| service.name.clone())
                    .unwrap_or_else(|| "deleted service".to_string()),
                deleted_at: service.and_then(|service| service.deleted_at.clone()),
                cpu_dollars: breakdown.cpu_dollars,
                memory_dollars: breakdown.memory_dollars,
                egress_dollars: breakdown.egress_dollars,
                volume_dollars: breakdown.volume_dollars,
                backup_dollars: breakdown.backup_dollars,
                total_dollars: breakdown.total_dollars,
            }
        })
        .filter(|service| service.total_dollars > 0.0)
        .collect::<Vec<_>>();

    summaries.sort_by(|a, b| {
        b.total_dollars
            .partial_cmp(&a.total_dollars)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    summaries
}

fn usage_item_from_aggregated(usages: &[AggregatedUsage]) -> UsageItem {
    UsageItem {
        memory_usage_gb: sum_usage(usages, "MEMORY_USAGE_GB"),
        cpu_percent_vcpu: sum_usage(usages, "CPU_USAGE"),
        egress_gb: sum_usage(usages, "NETWORK_TX_GB"),
        disk_gb: sum_usage(usages, "DISK_USAGE_GB"),
        backup_gb: sum_usage(usages, "BACKUP_USAGE_GB"),
    }
}

fn usage_item_from_estimated(usages: &[EstimatedUsage]) -> UsageItem {
    UsageItem {
        memory_usage_gb: sum_estimated(usages, "MEMORY_USAGE_GB"),
        cpu_percent_vcpu: sum_estimated(usages, "CPU_USAGE"),
        egress_gb: sum_estimated(usages, "NETWORK_TX_GB"),
        disk_gb: sum_estimated(usages, "DISK_USAGE_GB"),
        backup_gb: sum_estimated(usages, "BACKUP_USAGE_GB"),
    }
}

fn sum_usage(usages: &[AggregatedUsage], measurement: &str) -> f64 {
    usages
        .iter()
        .filter(|usage| usage.measurement == measurement)
        .map(|usage| usage.value)
        .sum()
}

fn sum_estimated(usages: &[EstimatedUsage], measurement: &str) -> f64 {
    usages
        .iter()
        .filter(|usage| usage.measurement == measurement)
        .map(|usage| usage.estimated_value)
        .sum()
}

fn cost_breakdown(item: &UsageItem) -> UsageCostBreakdown {
    UsageCostBreakdown {
        cpu_dollars: item.cpu_percent_vcpu * PRICE_MINUTELY_VCPU,
        memory_dollars: item.memory_usage_gb * PRICE_MINUTELY_MEM_GB,
        egress_dollars: item.egress_gb * PRICE_EGRESS_GB,
        volume_dollars: item.disk_gb * PRICE_MINUTELY_DISK_GB,
        backup_dollars: item.backup_gb * PRICE_MINUTELY_BACKUP_GB,
        total_dollars: cost_for_usage_item(item),
    }
}

fn cost_for_usage_item(item: &UsageItem) -> f64 {
    item.memory_usage_gb * PRICE_MINUTELY_MEM_GB
        + item.cpu_percent_vcpu * PRICE_MINUTELY_VCPU
        + item.egress_gb * PRICE_EGRESS_GB
        + item.disk_gb * PRICE_MINUTELY_DISK_GB
        + item.backup_gb * PRICE_MINUTELY_BACKUP_GB
}

fn line_items_from_usage_item(item: &UsageItem) -> Vec<UsageLineItem> {
    let breakdown = cost_breakdown(item);
    [
        ("CPU", breakdown.cpu_dollars),
        ("Memory", breakdown.memory_dollars),
        ("Egress", breakdown.egress_dollars),
        ("Volume", breakdown.volume_dollars),
        ("Backup", breakdown.backup_dollars),
    ]
    .into_iter()
    .filter(|(_, current_usage_dollars)| *current_usage_dollars > 0.0)
    .map(|(label, current_usage_dollars)| UsageLineItem {
        label: label.to_string(),
        current_usage_dollars,
    })
    .collect()
}

fn confirm_remove_usage_limit(yes: bool, workspace_name: &str) -> Result<()> {
    let confirmed = if yes {
        true
    } else if std::io::stdout().is_terminal() {
        prompt_confirm_with_default(
            &format!("Remove compute usage limits for workspace {workspace_name}?"),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirmed {
        bail!("Usage limit removal cancelled");
    }

    Ok(())
}

fn usage_limit_set_request(
    soft: Option<LimitAmount>,
    hard: Option<LimitAmount>,
    existing: Option<&UsageLimitSummary>,
) -> Result<UsageLimitSetRequest> {
    if soft.is_none() && hard.is_none() {
        bail!("At least one of --soft or --hard is required");
    }

    let request = UsageLimitSetRequest {
        soft_limit: match soft {
            Some(soft) => soft.whole_dollars("Compute email alert")?,
            None => existing.map(|limit| limit.soft_limit).unwrap_or(0),
        },
        hard_limit: match hard {
            Some(hard) => Some(hard.whole_dollars("Compute hard limit")?),
            None => existing.and_then(|limit| limit.hard_limit),
        },
    };

    validate_limit_values(request.soft_limit, request.hard_limit)?;

    Ok(request)
}

fn validate_limit_values(soft: u32, hard: Option<u32>) -> Result<()> {
    if soft != 0 && soft < MIN_SOFT_USAGE_LIMIT_DOLLARS {
        bail!("Compute email alert must be at least $5, or exactly $0");
    }

    if soft > MAX_USAGE_LIMIT_DOLLARS {
        bail!("Compute email alert must be at most $500,000");
    }

    if let Some(hard) = hard.filter(|hard| *hard != 0) {
        if hard < MIN_HARD_USAGE_LIMIT_DOLLARS {
            bail!("Compute hard limit must be at least $10, or exactly $0");
        }

        if hard > MAX_USAGE_LIMIT_DOLLARS {
            bail!("Compute hard limit must be at most $500,000");
        }

        if hard < soft {
            bail!("Compute hard limit must be greater than or equal to compute email alert");
        }
    }

    Ok(())
}

fn agent_usage_limit_set_request(args: &SetLimitArgs) -> Result<AgentUsageLimitSetRequest> {
    let hard = args
        .hard
        .ok_or_else(|| anyhow::anyhow!("Agent hard limit is required"))?;
    let hard_limit_cents = hard.cents("Agent hard limit")?;
    let soft_limit_cents = args
        .soft
        .map(|soft| soft.cents("Agent email alert"))
        .transpose()?;

    validate_agent_limit_values(soft_limit_cents, hard_limit_cents)?;

    Ok(AgentUsageLimitSetRequest {
        soft_limit_cents,
        hard_limit_cents,
    })
}

fn validate_agent_limit_values(soft: Option<u32>, hard: u32) -> Result<()> {
    let max_cents = MAX_USAGE_LIMIT_DOLLARS * 100;

    if hard > max_cents {
        bail!("Agent hard limit must be at most $500,000.");
    }

    if let Some(soft) = soft {
        if soft > max_cents {
            bail!("Agent email alert must be at most $500,000.");
        }

        if soft > hard {
            bail!("Agent hard limit must be greater than or equal to agent email alert.");
        }
    }

    Ok(())
}

fn print_usage_summary(summary: &WorkspaceUsageSummary) {
    println!("{}", "Workspace usage".bold());
    println!();
    print_field("Workspace:", &summary.workspace.name, FIELD_LABEL_WIDTH);
    print_field(
        "Billing period:",
        &format_billing_period(&summary.billing_period),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Current usage:",
        &format_money(summary.current_usage_dollars),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Current bill:",
        &format_money(summary.current_bill_dollars),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Estimated bill:",
        &summary
            .estimated_bill_dollars
            .map(format_money)
            .unwrap_or_else(|| "n/a".to_string()),
        FIELD_LABEL_WIDTH,
    );
    print_usage_limit_fields(summary.usage_limit.as_ref());
}

fn print_projects(summary: &WorkspaceUsageSummary, limit: usize) {
    let output = limited_projects(summary, Some(limit));

    println!("{}", "Usage by project".bold());
    println!();
    print_field("Workspace:", &summary.workspace.name, 22);
    print_field(
        "Billing period:",
        &format_billing_period(&summary.billing_period),
        22,
    );
    print_field("Projects with usage:", &output.project_count, 22);
    print_field(
        "Total usage:",
        &format_money(output.total_usage_dollars),
        22,
    );
    let showing = if output.truncated {
        format!("top {} by usage", output.projects.len())
    } else {
        "all projects".to_string()
    };
    print_field("Showing:", &showing, 22);
    println!();

    if output.projects.is_empty() {
        println!("No project usage for this period.");
        return;
    }

    let project_name_width = project_name_column_width(&output);

    println!(
        "{:<width$} {:>cost_width$}",
        "Project".dimmed(),
        "Current Cost".dimmed(),
        width = project_name_width,
        cost_width = CURRENT_COST_COLUMN_WIDTH,
    );

    for project in &output.projects {
        println!(
            "{:<width$} {:>cost_width$}",
            project_table_name(&project.name, project_name_width),
            format_money(project.current_usage_dollars),
            width = project_name_width,
            cost_width = CURRENT_COST_COLUMN_WIDTH,
        );

        if let Some(deleted_at) = project.deleted_at.as_deref() {
            println!("  {}", deleted_on_label(deleted_at).dimmed());
        }
    }

    if let Some(other) = &output.other_projects {
        println!(
            "{:<width$} {:>cost_width$}",
            truncate_chars(
                &format!("Other projects ({})", other.count),
                project_name_width
            ),
            format_money(other.current_usage_dollars),
            width = project_name_width,
            cost_width = CURRENT_COST_COLUMN_WIDTH,
        );
    }
}

fn print_project_usage(summary: &ProjectUsageSummary) {
    println!("{}", "Project usage".bold());
    println!();
    print_field("Workspace:", &summary.workspace.name, FIELD_LABEL_WIDTH);
    print_field(
        "Project:",
        &deleted_name(&summary.project.name, summary.project.deleted_at.as_deref()),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Billing period:",
        &format_billing_period(&summary.billing_period),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Usage:",
        &format_money(summary.current_usage_dollars),
        FIELD_LABEL_WIDTH,
    );
    println!();

    if summary.services.is_empty() {
        println!("No service usage for this period.");
        return;
    }

    println!(
        "{:<SERVICE_NAME_WIDTH$} {:>8} {:>9} {:>8} {:>8} {:>8} {:>8}",
        "Service".dimmed(),
        "CPU".dimmed(),
        "Memory".dimmed(),
        "Egress".dimmed(),
        "Volume".dimmed(),
        "Backup".dimmed(),
        "Total".dimmed()
    );

    for service in &summary.services {
        println!(
            "{:<SERVICE_NAME_WIDTH$} {:>8} {:>9} {:>8} {:>8} {:>8} {:>8}",
            deleted_name(&service.name, service.deleted_at.as_deref()),
            format_money(service.cpu_dollars),
            format_money(service.memory_dollars),
            format_money(service.egress_dollars),
            format_money(service.volume_dollars),
            format_money(service.backup_dollars),
            format_money(service.total_dollars),
        );
    }
}

fn print_limit_status(summary: &WorkspaceUsageSummary) {
    println!("{}", "Usage limit".bold());
    println!();
    print_field("Workspace:", &summary.workspace.name, FIELD_LABEL_WIDTH);
    print_field(
        "Current usage:",
        &format_money(summary.current_usage_dollars),
        FIELD_LABEL_WIDTH,
    );
    print_usage_limit_fields(summary.usage_limit.as_ref());
}

fn print_limit_action(action: &str, summary: &WorkspaceUsageSummary) {
    let title = match action {
        "created" => "Usage limit created",
        "updated" => "Usage limit updated",
        "removed" => "Usage limit removed",
        _ => "Usage limit",
    };

    println!("{}", title.bold());
    println!();
    print_field("Workspace:", &summary.workspace.name, FIELD_LABEL_WIDTH);
    print_field(
        "Current usage:",
        &format_money(summary.current_usage_dollars),
        FIELD_LABEL_WIDTH,
    );
    print_usage_limit_fields(summary.usage_limit.as_ref());
}

fn print_combined_limit_status(summary: &WorkspaceUsageSummary, agent_usage: &AgentUsageSummary) {
    println!("{}", "Usage limits".bold());
    println!();
    print_field("Workspace:", &summary.workspace.name, FIELD_LABEL_WIDTH);
    println!();
    println!("{}", "Workspace".bold());
    print_field(
        "Current usage:",
        &format_money(summary.current_usage_dollars),
        FIELD_LABEL_WIDTH,
    );
    print_usage_limit_fields(summary.usage_limit.as_ref());
    println!();
    println!("{}", "Agent".bold());
    print_agent_usage_fields(agent_usage);
}

fn print_agent_limit_status(workspace: &Workspace, agent_usage: &AgentUsageSummary) {
    println!("{}", "Agent usage limit".bold());
    println!();
    print_field("Workspace:", &workspace.name(), FIELD_LABEL_WIDTH);
    print_agent_usage_fields(agent_usage);
}

fn print_agent_limit_action(action: &str, workspace: &Workspace, agent_usage: &AgentUsageSummary) {
    let title = match action {
        "updated" => "Agent usage limit updated",
        _ => "Agent usage limit",
    };

    println!("{}", title.bold());
    println!();
    print_field("Workspace:", &workspace.name(), FIELD_LABEL_WIDTH);
    print_agent_usage_fields(agent_usage);
}

fn print_usage_limit_fields(limit: Option<&UsageLimitSummary>) {
    match limit {
        Some(limit) => {
            print_field(
                "Soft limit:",
                &format_soft_limit(limit.soft_limit),
                FIELD_LABEL_WIDTH,
            );
            print_field(
                "Hard limit:",
                &limit
                    .hard_limit
                    .map(format_whole_dollars)
                    .unwrap_or_else(|| "not set".to_string()),
                FIELD_LABEL_WIDTH,
            );
            print_field(
                "Over limit:",
                &yes_no(limit.is_over_limit),
                FIELD_LABEL_WIDTH,
            );
        }
        None => {
            print_field("Soft limit:", &"not set", FIELD_LABEL_WIDTH);
            print_field("Hard limit:", &"not set", FIELD_LABEL_WIDTH);
            print_field("Over limit:", &"no", FIELD_LABEL_WIDTH);
        }
    }
}

fn print_agent_usage_fields(agent_usage: &AgentUsageSummary) {
    print_field(
        "Current usage:",
        &format_cents(agent_usage.total_used_cents),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Period end:",
        &format_iso_date(&agent_usage.billing_period_end),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Soft limit:",
        &format_agent_soft_limit(agent_usage.soft_limit_cents),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Hard limit:",
        &format_agent_hard_limit(agent_usage.hard_limit_cents),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Remaining:",
        &format_agent_remaining(agent_usage),
        FIELD_LABEL_WIDTH,
    );
}

fn print_usage_summary_json(summary: &WorkspaceUsageSummary) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&usage_summary_json(summary))?
    );
    Ok(())
}

fn print_projects_json(summary: &WorkspaceUsageSummary, limit: Option<usize>) -> Result<()> {
    let output = limited_projects(summary, limit);
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn print_project_usage_json(summary: &ProjectUsageSummary) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&project_usage_json(summary))?
    );
    Ok(())
}

fn print_limit_json(action: &str, summary: &WorkspaceUsageSummary) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&limit_json(action, summary))?
    );
    Ok(())
}

fn print_agent_limit_json(
    action: &str,
    workspace: &Workspace,
    agent_usage: &AgentUsageSummary,
) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&agent_limit_json(action, workspace, agent_usage))?
    );
    Ok(())
}

fn print_combined_limit_json(
    summary: &WorkspaceUsageSummary,
    agent_usage: &AgentUsageSummary,
) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&combined_limit_json(summary, agent_usage))?
    );
    Ok(())
}

fn usage_summary_json(summary: &WorkspaceUsageSummary) -> serde_json::Value {
    serde_json::json!({
        "workspace": summary.workspace,
        "customer": summary.customer,
        "billingPeriod": summary.billing_period,
        "period": summary.period,
        "isCurrentPeriod": summary.is_current_period,
        "currentUsageDollars": summary.current_usage_dollars,
        "currentBillDollars": summary.current_bill_dollars,
        "estimatedBillDollars": summary.estimated_bill_dollars,
        "usageLimit": summary.usage_limit,
        "lineItems": summary.line_items,
    })
}

fn project_usage_json(summary: &ProjectUsageSummary) -> serde_json::Value {
    let services = summary
        .services
        .iter()
        .map(|service| {
            serde_json::json!({
                "id": service.id,
                "name": service.name,
                "cpuDollars": service.cpu_dollars,
                "memoryDollars": service.memory_dollars,
                "egressDollars": service.egress_dollars,
                "volumeDollars": service.volume_dollars,
                "backupDollars": service.backup_dollars,
                "totalDollars": service.total_dollars,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "workspace": summary.workspace,
        "project": summary.project,
        "billingPeriod": summary.billing_period,
        "period": summary.period,
        "currentUsageDollars": summary.current_usage_dollars,
        "services": services,
    })
}

fn limit_json(action: &str, summary: &WorkspaceUsageSummary) -> serde_json::Value {
    serde_json::json!({
        "action": action,
        "workspace": summary.workspace,
        "customer": summary.customer,
        "currentUsageDollars": summary.current_usage_dollars,
        "usageLimit": summary.usage_limit,
    })
}

fn agent_limit_json(
    action: &str,
    workspace: &Workspace,
    agent_usage: &AgentUsageSummary,
) -> serde_json::Value {
    serde_json::json!({
        "action": action,
        "workspace": summary_workspace_from_workspace(workspace),
        "agentUsage": agent_usage_json(agent_usage),
    })
}

fn combined_limit_json(
    summary: &WorkspaceUsageSummary,
    agent_usage: &AgentUsageSummary,
) -> serde_json::Value {
    serde_json::json!({
        "action": "status",
        "workspace": summary.workspace,
        "customer": summary.customer,
        "workspaceUsage": {
            "currentUsageDollars": summary.current_usage_dollars,
            "usageLimit": summary.usage_limit,
        },
        "agentUsage": agent_usage_json(agent_usage),
    })
}

fn agent_usage_json(agent_usage: &AgentUsageSummary) -> serde_json::Value {
    serde_json::json!({
        "totalUsedCents": agent_usage.total_used_cents,
        "totalUsedDollars": cents_to_dollars(agent_usage.total_used_cents),
        "hardLimitCents": agent_usage.hard_limit_cents,
        "hardLimitDollars": agent_usage.hard_limit_cents.map(cents_to_dollars),
        "softLimitCents": agent_usage.soft_limit_cents,
        "softLimitDollars": agent_usage.soft_limit_cents.map(cents_to_dollars),
        "usageRemaining": agent_usage.usage_remaining,
        "billingPeriodEnd": agent_usage.billing_period_end,
    })
}

fn limited_projects(summary: &WorkspaceUsageSummary, limit: Option<usize>) -> ProjectsOutput {
    let mut projects = summary.projects.clone();
    sort_workspace_usage_projects(&mut projects);

    let total_usage_dollars = total_project_usage_dollars(&projects);
    let project_count = projects.len();
    let effective_limit = limit.unwrap_or(project_count);
    let returned = projects
        .iter()
        .take(effective_limit)
        .cloned()
        .collect::<Vec<_>>();
    let truncated = returned.len() < project_count;
    let other_projects = if truncated {
        let current_usage_dollars = projects
            .iter()
            .skip(effective_limit)
            .map(|project| project.current_usage_dollars)
            .sum();
        Some(OtherProjects {
            count: project_count - returned.len(),
            current_usage_dollars,
            share: share(current_usage_dollars, total_usage_dollars),
        })
    } else {
        None
    };

    ProjectsOutput {
        workspace: summary.workspace.clone(),
        billing_period: summary.billing_period.clone(),
        period: summary.period.clone(),
        total_usage_dollars,
        project_count,
        returned_project_count: returned.len(),
        truncated,
        other_projects,
        projects: returned,
    }
}

fn sort_workspace_usage_projects(projects: &mut [WorkspaceUsageProject]) {
    projects.sort_by(|a, b| {
        a.deleted_at
            .is_some()
            .cmp(&b.deleted_at.is_some())
            .then_with(|| {
                b.current_usage_dollars
                    .partial_cmp(&a.current_usage_dollars)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn project_name_column_width(output: &ProjectsOutput) -> usize {
    let project_width = output
        .projects
        .iter()
        .map(|project| project.name.chars().count())
        .chain(
            output
                .other_projects
                .as_ref()
                .map(|other| format!("Other projects ({})", other.count).chars().count()),
        )
        .max()
        .unwrap_or("Project".len());

    project_width.clamp(PROJECT_NAME_MIN_WIDTH, PROJECT_NAME_MAX_WIDTH)
}

fn project_table_name(name: &str, max_width: usize) -> String {
    truncate_chars(name, max_width)
}

fn truncate_chars(value: &str, max_width: usize) -> String {
    if value.chars().count() <= max_width {
        value.to_string()
    } else if max_width <= 3 {
        ".".repeat(max_width)
    } else {
        format!(
            "{}...",
            value
                .chars()
                .take(max_width.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn total_project_usage_dollars(projects: &[WorkspaceUsageProject]) -> f64 {
    projects
        .iter()
        .map(|project| project.current_usage_dollars)
        .sum()
}

fn share(current_usage_dollars: f64, total_usage_dollars: f64) -> f64 {
    if total_usage_dollars == 0.0 {
        0.0
    } else {
        round_share(current_usage_dollars / total_usage_dollars)
    }
}

fn parse_period(period: &str) -> std::result::Result<String, String> {
    if period == "current" || period == "previous" || is_year_month(period) {
        Ok(period.to_string())
    } else {
        Err("period must be current, previous, or YYYY-MM".to_string())
    }
}

fn is_year_month(value: &str) -> bool {
    let Some((year, month)) = value.split_once('-') else {
        return false;
    };

    year.len() == 4
        && month.len() == 2
        && year.chars().all(|c| c.is_ascii_digit())
        && month
            .parse::<u8>()
            .is_ok_and(|month| (1..=12).contains(&month))
}

fn parse_limit(limit: &str) -> std::result::Result<usize, String> {
    match limit.parse::<usize>() {
        Ok(limit) if limit > 0 => Ok(limit),
        _ => Err("limit must be a positive integer".to_string()),
    }
}

fn parse_limit_amount(value: &str) -> std::result::Result<LimitAmount, String> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') || value.starts_with('+') {
        return Err("limit must be a non-negative dollar amount".to_string());
    }

    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if parts.next().is_some() {
        return Err("limit must be a dollar amount with at most two decimal places".to_string());
    }

    let fraction = fraction.unwrap_or_default();
    if whole.is_empty() && fraction.is_empty() {
        return Err("limit must be a non-negative dollar amount".to_string());
    }

    if !whole.chars().all(|c| c.is_ascii_digit())
        || !fraction.chars().all(|c| c.is_ascii_digit())
        || fraction.len() > 2
    {
        return Err("limit must be a dollar amount with at most two decimal places".to_string());
    }

    let whole_dollars = if whole.is_empty() {
        0
    } else {
        whole
            .parse::<u64>()
            .map_err(|_| "limit is too large".to_string())?
    };
    let whole_cents = whole_dollars
        .checked_mul(100)
        .ok_or_else(|| "limit is too large".to_string())?;
    let fractional_cents = match fraction.len() {
        0 => 0,
        1 => {
            fraction.parse::<u64>().map_err(|_| {
                "limit must be a dollar amount with at most two decimal places".to_string()
            })? * 10
        }
        2 => fraction.parse::<u64>().map_err(|_| {
            "limit must be a dollar amount with at most two decimal places".to_string()
        })?,
        _ => unreachable!(),
    };

    Ok(LimitAmount {
        cents: whole_cents
            .checked_add(fractional_cents)
            .ok_or_else(|| "limit is too large".to_string())?,
    })
}

fn format_billing_period(period: &BillingPeriod) -> String {
    format!(
        "{} - {}",
        format_iso_date(&period.start),
        format_iso_date(&period.end)
    )
}

fn format_iso_date(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.format("%b %-d, %Y").to_string())
        .unwrap_or_else(|_| value.to_string())
}

fn format_money(value: f64) -> String {
    let precision = if value.abs() < 1.0 { 4 } else { 2 };
    let formatted = format!("{value:.precision$}");
    let Some((whole, fractional)) = formatted.split_once('.') else {
        return format!("${formatted}");
    };
    let sign = if whole.starts_with('-') { "-" } else { "" };
    let whole = whole.trim_start_matches('-').parse::<u64>().unwrap_or(0);

    format!("{sign}${}.{}", format_number_with_commas(whole), fractional)
}

fn format_cents(cents: u32) -> String {
    let whole = cents / 100;
    let fractional = cents % 100;
    format!(
        "${}.{:02}",
        format_number_with_commas(whole.into()),
        fractional
    )
}

fn cents_to_dollars(cents: u32) -> f64 {
    cents as f64 / 100.0
}

fn format_whole_dollars(value: u32) -> String {
    format!("${}", format_number_with_commas(value.into()))
}

fn format_soft_limit(value: u32) -> String {
    if value == 0 {
        "not set".to_string()
    } else {
        format_whole_dollars(value)
    }
}

fn format_agent_soft_limit(value: Option<u32>) -> String {
    match value {
        Some(value) if value > 0 => format_cents(value),
        _ => "not set".to_string(),
    }
}

fn format_agent_hard_limit(value: Option<u32>) -> String {
    match value {
        Some(0) => "$0.00 (blocked)".to_string(),
        Some(value) => format_cents(value),
        None => "unlimited".to_string(),
    }
}

fn format_agent_remaining(agent_usage: &AgentUsageSummary) -> String {
    match (agent_usage.hard_limit_cents, agent_usage.usage_remaining) {
        (Some(0), _) => "blocked".to_string(),
        (_, Some(remaining)) => format!("{:.1}%", (remaining * 1000.0).round() / 10.0),
        _ => "n/a".to_string(),
    }
}

fn format_number_with_commas(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn round_share(value: f64) -> f64 {
    ((value + f64::EPSILON) * 1000.0).round() / 1000.0
}

fn deleted_name(name: &str, deleted_at: Option<&str>) -> String {
    if deleted_at.is_some() {
        format!("{name} (deleted)")
    } else {
        name.to_string()
    }
}

fn deleted_on_label(deleted_at: &str) -> String {
    format!("Deleted on {}", format_iso_date(deleted_at))
}

fn yes_no(value: bool) -> impl Display {
    if value { "yes" } else { "no" }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum LimitTarget {
    Agent,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LimitAmount {
    cents: u64,
}

impl LimitAmount {
    fn whole_dollars(self, label: &str) -> Result<u32> {
        if self.cents % 100 != 0 {
            bail!("{label} must be a whole dollar amount");
        }

        u32::try_from(self.cents / 100).map_err(|_| anyhow::anyhow!("{label} is too large"))
    }

    fn cents(self, label: &str) -> Result<u32> {
        u32::try_from(self.cents).map_err(|_| anyhow::anyhow!("{label} is too large"))
    }
}

#[derive(Debug)]
struct ProjectMatch {
    id: String,
    name: String,
}

#[derive(Debug, Clone)]
struct ResolvedUsagePeriod {
    billing_period: BillingPeriod,
    period: String,
    is_current_period: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageItem {
    cpu_percent_vcpu: f64,
    memory_usage_gb: f64,
    egress_gb: f64,
    disk_gb: f64,
    backup_gb: f64,
}

#[derive(Debug, Clone, Copy)]
struct UsageCostBreakdown {
    cpu_dollars: f64,
    memory_dollars: f64,
    egress_dollars: f64,
    volume_dollars: f64,
    backup_dollars: f64,
    total_dollars: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceUsageContextResponse {
    workspace: WorkspaceUsageContext,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceUsageContext {
    id: String,
    name: String,
    customer: WorkspaceCustomer,
}

impl WorkspaceUsageContext {
    fn workspace(&self) -> SummaryWorkspace {
        SummaryWorkspace {
            id: self.id.clone(),
            name: self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceCustomer {
    id: String,
    current_usage: Option<f64>,
    billing_period: BillingPeriod,
    usage_limit: Option<UsageLimitSummary>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceUsageResponse {
    usage: Vec<AggregatedUsage>,
    projects: ProjectConnection,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceEstimatedUsageResponse {
    estimated_usage: Vec<EstimatedUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentUsageResponse {
    agent_usage: AgentUsageSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentUsageSummary {
    total_used_cents: u32,
    hard_limit_cents: Option<u32>,
    soft_limit_cents: Option<u32>,
    usage_remaining: Option<f64>,
    billing_period_end: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AggregatedUsage {
    measurement: String,
    value: f64,
    tags: UsageTags,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageTags {
    project_id: Option<String>,
    service_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EstimatedUsage {
    measurement: String,
    estimated_value: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectConnection {
    edges: Vec<ProjectEdge>,
}

impl ProjectConnection {
    fn nodes(&self) -> Vec<WorkspaceUsageProjectNode> {
        self.edges.iter().map(|edge| edge.node.clone()).collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectEdge {
    node: WorkspaceUsageProjectNode,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceUsageProjectNode {
    id: String,
    name: String,
    deleted_at: Option<String>,
    services: Option<ServiceConnection>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectUsageResponse {
    usage: Vec<AggregatedUsage>,
    projects: ProjectConnection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceConnection {
    edges: Vec<ServiceEdge>,
}

impl ServiceConnection {
    fn nodes(&self) -> Vec<ProjectUsageServiceNode> {
        self.edges.iter().map(|edge| edge.node.clone()).collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceEdge {
    node: ProjectUsageServiceNode,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectUsageServiceNode {
    id: String,
    name: String,
    deleted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummaryWorkspace {
    id: String,
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummaryCustomer {
    id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BillingPeriod {
    start: String,
    end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageLimitSummary {
    soft_limit: u32,
    hard_limit: Option<u32>,
    is_over_limit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageLineItem {
    label: String,
    current_usage_dollars: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceUsageProject {
    id: String,
    name: String,
    deleted_at: Option<String>,
    current_usage_dollars: f64,
    share: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceUsageSummary {
    workspace: SummaryWorkspace,
    customer: SummaryCustomer,
    billing_period: BillingPeriod,
    period: String,
    is_current_period: bool,
    current_usage_dollars: f64,
    current_bill_dollars: f64,
    estimated_bill_dollars: Option<f64>,
    usage_limit: Option<UsageLimitSummary>,
    line_items: Vec<UsageLineItem>,
    projects: Vec<WorkspaceUsageProject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectUsageProject {
    id: String,
    name: String,
    deleted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectUsageService {
    id: String,
    name: String,
    deleted_at: Option<String>,
    cpu_dollars: f64,
    memory_dollars: f64,
    egress_dollars: f64,
    volume_dollars: f64,
    backup_dollars: f64,
    total_dollars: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectUsageSummary {
    workspace: SummaryWorkspace,
    project: ProjectUsageProject,
    billing_period: BillingPeriod,
    period: String,
    current_usage_dollars: f64,
    services: Vec<ProjectUsageService>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectsOutput {
    workspace: SummaryWorkspace,
    billing_period: BillingPeriod,
    period: String,
    total_usage_dollars: f64,
    project_count: usize,
    returned_project_count: usize,
    truncated: bool,
    other_projects: Option<OtherProjects>,
    projects: Vec<WorkspaceUsageProject>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OtherProjects {
    count: usize,
    current_usage_dollars: f64,
    share: f64,
}

#[derive(Debug, PartialEq)]
struct UsageLimitSetRequest {
    soft_limit: u32,
    hard_limit: Option<u32>,
}

#[derive(Debug, PartialEq)]
struct AgentUsageLimitSetRequest {
    soft_limit_cents: Option<u32>,
    hard_limit_cents: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dollars(value: u64) -> LimitAmount {
        cents(value * 100)
    }

    fn cents(value: u64) -> LimitAmount {
        LimitAmount { cents: value }
    }

    #[test]
    fn parses_usage_projects_with_filters() {
        let args = Args::try_parse_from([
            "usage",
            "projects",
            "--project",
            "api",
            "--period",
            "2026-07",
        ])
        .unwrap();

        match args.command.unwrap() {
            Commands::Projects(projects) => {
                assert_eq!(projects.project.as_deref(), Some("api"));
                assert_eq!(projects.period.as_deref(), Some("2026-07"));
                assert_eq!(projects.limit, None);
            }
            _ => panic!("expected projects command"),
        }

        let args = Args::try_parse_from(["usage", "projects", "--limit", "10"]).unwrap();
        match args.command.unwrap() {
            Commands::Projects(projects) => {
                assert_eq!(projects.project.as_deref(), None);
                assert_eq!(projects.limit, Some(10));
            }
            _ => panic!("expected projects command"),
        }
    }

    #[test]
    fn parses_usage_limit_set_and_update() {
        let set = Args::try_parse_from([
            "usage",
            "limit",
            "set",
            "--target",
            "workspace",
            "--soft",
            "75",
            "--hard",
            "125",
            "--workspace",
            "Acme",
        ])
        .unwrap();
        assert_eq!(set.workspace.as_deref(), Some("Acme"));

        match set.command.unwrap() {
            Commands::Limit(limit_args) => match limit_args.command {
                LimitCommands::Set(set_args) => {
                    assert_eq!(set_args.target, LimitTarget::Workspace);
                    assert_eq!(set_args.soft, Some(dollars(75)));
                    assert_eq!(set_args.hard, Some(dollars(125)));
                }
                _ => panic!("expected set command"),
            },
            _ => panic!("expected limit command"),
        }

        let agent = Args::try_parse_from([
            "usage", "limit", "set", "--target", "agent", "--soft", "7.50", "--hard", "20",
        ])
        .unwrap();
        match agent.command.unwrap() {
            Commands::Limit(limit_args) => match limit_args.command {
                LimitCommands::Set(set_args) => {
                    assert_eq!(set_args.target, LimitTarget::Agent);
                    assert_eq!(set_args.soft, Some(cents(750)));
                    assert_eq!(set_args.hard, Some(dollars(20)));
                }
                _ => panic!("expected set command"),
            },
            _ => panic!("expected limit command"),
        }

        let update = Args::try_parse_from(["usage", "limit", "update", "--soft", "75"]).unwrap();
        match update.command.unwrap() {
            Commands::Limit(limit_args) => match limit_args.command {
                LimitCommands::Update(set_args) => {
                    assert_eq!(set_args.soft, Some(dollars(75)));
                    assert_eq!(set_args.hard, None);
                }
                _ => panic!("expected update command"),
            },
            _ => panic!("expected limit command"),
        }

        let hard_only = Args::try_parse_from([
            "usage",
            "limit",
            "set",
            "--target",
            "workspace",
            "--hard",
            "125",
        ])
        .unwrap();
        match hard_only.command.unwrap() {
            Commands::Limit(limit_args) => match limit_args.command {
                LimitCommands::Set(set_args) => {
                    assert_eq!(set_args.soft, None);
                    assert_eq!(set_args.hard, Some(dollars(125)));
                }
                _ => panic!("expected set command"),
            },
            _ => panic!("expected limit command"),
        }
    }

    #[test]
    fn parses_usage_limit_status_target() {
        let default = Args::try_parse_from(["usage", "limit", "status"]).unwrap();
        match default.command.unwrap() {
            Commands::Limit(limit_args) => match limit_args.command {
                LimitCommands::Status(status_args) => assert_eq!(status_args.target, None),
                _ => panic!("expected status command"),
            },
            _ => panic!("expected limit command"),
        }

        let agent =
            Args::try_parse_from(["usage", "limit", "status", "--target", "agent"]).unwrap();
        match agent.command.unwrap() {
            Commands::Limit(limit_args) => match limit_args.command {
                LimitCommands::Status(status_args) => {
                    assert_eq!(status_args.target, Some(LimitTarget::Agent));
                }
                _ => panic!("expected status command"),
            },
            _ => panic!("expected limit command"),
        }
    }

    #[test]
    fn parses_usage_limit_remove() {
        let args = Args::try_parse_from(["usage", "limit", "remove", "--yes", "--json"]).unwrap();

        assert!(args.json);
        match args.command.unwrap() {
            Commands::Limit(limit_args) => match limit_args.command {
                LimitCommands::Remove(remove_args) => assert!(remove_args.yes),
                _ => panic!("expected remove command"),
            },
            _ => panic!("expected limit command"),
        }
    }

    #[test]
    fn rejects_usage_list_and_bad_period() {
        assert!(Args::try_parse_from(["usage", "list"]).is_err());
        assert!(Args::try_parse_from(["usage", "limit"]).is_err());
        assert!(Args::try_parse_from(["usage", "limit", "set"]).is_err());
        assert!(Args::try_parse_from(["usage", "limit", "set", "--hard", "125"]).is_err());
        assert!(Args::try_parse_from(["usage", "limit", "update"]).is_err());
        assert!(Args::try_parse_from(["usage", "--period", "tomorrow"]).is_err());
        assert!(Args::try_parse_from(["usage", "projects", "--period", "2026-13"]).is_err());
        assert!(
            Args::try_parse_from(["usage", "projects", "--project", "api", "--limit", "10"])
                .is_err()
        );
    }

    #[test]
    fn validates_usage_limit_set_inputs() {
        assert!(validate_limit_values(0, Some(125)).is_ok());
        assert!(validate_limit_values(75, None).is_ok());
        assert!(validate_limit_values(0, Some(0)).is_ok());
        assert!(validate_limit_values(0, Some(9)).is_err());
        assert!(validate_limit_values(4, None).is_err());
        assert!(validate_limit_values(150, Some(125)).is_err());
    }

    #[test]
    fn validates_agent_usage_limit_set_inputs() {
        assert!(validate_agent_limit_values(Some(750), 2000).is_ok());
        assert!(validate_agent_limit_values(None, 0).is_ok());
        assert!(validate_agent_limit_values(Some(2001), 2000).is_err());
        assert!(validate_agent_limit_values(None, 50_000_001).is_err());

        let request = agent_usage_limit_set_request(&SetLimitArgs {
            target: LimitTarget::Agent,
            soft: Some(cents(750)),
            hard: Some(dollars(20)),
        })
        .unwrap();
        assert_eq!(
            request,
            AgentUsageLimitSetRequest {
                soft_limit_cents: Some(750),
                hard_limit_cents: 2000,
            }
        );

        assert!(
            agent_usage_limit_set_request(&SetLimitArgs {
                target: LimitTarget::Agent,
                soft: Some(dollars(5)),
                hard: None,
            })
            .is_err()
        );
    }

    #[test]
    fn usage_limit_set_request_preserves_omitted_existing_limits() {
        let summary = sample_workspace_summary();
        let existing = summary.usage_limit.as_ref();

        assert_eq!(
            usage_limit_set_request(None, Some(dollars(125)), existing,).unwrap(),
            UsageLimitSetRequest {
                soft_limit: 100,
                hard_limit: Some(125),
            },
        );

        assert_eq!(
            usage_limit_set_request(Some(dollars(75)), None, existing,).unwrap(),
            UsageLimitSetRequest {
                soft_limit: 75,
                hard_limit: Some(150),
            },
        );

        assert_eq!(
            usage_limit_set_request(Some(dollars(0)), None, existing,).unwrap(),
            UsageLimitSetRequest {
                soft_limit: 0,
                hard_limit: Some(150),
            },
        );

        assert_eq!(
            usage_limit_set_request(None, Some(dollars(0)), existing,).unwrap(),
            UsageLimitSetRequest {
                soft_limit: 100,
                hard_limit: Some(0),
            },
        );

        assert_eq!(
            usage_limit_set_request(None, Some(dollars(125)), None,).unwrap(),
            UsageLimitSetRequest {
                soft_limit: 0,
                hard_limit: Some(125),
            },
        );

        assert!(usage_limit_set_request(None, None, existing).is_err());
        assert!(usage_limit_set_request(Some(cents(7550)), None, existing).is_err());
    }

    #[test]
    fn projects_json_returns_all_by_default_and_rolls_up_when_limited() {
        let summary = sample_workspace_summary();

        let all = limited_projects(&summary, None);
        assert_eq!(all.project_count, 3);
        assert_eq!(all.returned_project_count, 3);
        assert!(!all.truncated);
        assert!(all.other_projects.is_none());

        let limited = limited_projects(&summary, Some(2));
        assert_eq!(limited.project_count, 3);
        assert_eq!(limited.returned_project_count, 2);
        assert!(limited.truncated);
        assert_eq!(limited.projects[0].name, "api-prod");
        assert_eq!(limited.projects[1].name, "web-prod");

        let other = limited.other_projects.unwrap();
        assert_eq!(other.count, 1);
        assert_eq!(other.current_usage_dollars, 20.0);
        assert_eq!(other.share, 0.1);
    }

    #[test]
    fn usage_summary_json_omits_project_breakdown() {
        let json = usage_summary_json(&sample_workspace_summary());

        assert_eq!(json["workspace"]["name"], "Acme");
        assert_eq!(json["customer"]["id"], "customer-id");
        assert_eq!(json["period"], "current");
        assert_eq!(json["usageLimit"]["softLimit"], 100);
        assert!(json.get("projects").is_none());
    }

    #[test]
    fn project_usage_json_matches_public_shape() {
        let json = project_usage_json(&sample_project_summary());

        assert_eq!(json["project"]["name"], "api");
        assert_eq!(json["currentUsageDollars"], 28.42);
        assert_eq!(json["services"][0]["name"], "api-server");
        assert_eq!(json["services"][0]["cpuDollars"], 8.4);
        assert!(json["services"][0].get("deletedAt").is_none());
    }

    #[test]
    fn limit_json_matches_action_shape() {
        let json = limit_json("updated", &sample_workspace_summary());

        assert_eq!(json["action"], "updated");
        assert_eq!(json["workspace"]["id"], "workspace-id");
        assert_eq!(json["customer"]["id"], "customer-id");
        assert_eq!(json["currentUsageDollars"], 200.0);
        assert_eq!(json["usageLimit"]["hardLimit"], 150);
    }

    fn sample_workspace_summary() -> WorkspaceUsageSummary {
        WorkspaceUsageSummary {
            workspace: SummaryWorkspace {
                id: "workspace-id".to_string(),
                name: "Acme".to_string(),
            },
            customer: SummaryCustomer {
                id: "customer-id".to_string(),
            },
            billing_period: BillingPeriod {
                start: "2026-07-01T00:00:00Z".to_string(),
                end: "2026-08-01T00:00:00Z".to_string(),
            },
            period: "current".to_string(),
            is_current_period: true,
            current_usage_dollars: 200.0,
            current_bill_dollars: 200.0,
            estimated_bill_dollars: Some(300.0),
            usage_limit: Some(UsageLimitSummary {
                soft_limit: 100,
                hard_limit: Some(150),
                is_over_limit: true,
            }),
            line_items: vec![UsageLineItem {
                label: "CPU".to_string(),
                current_usage_dollars: 12.34,
            }],
            projects: vec![
                WorkspaceUsageProject {
                    id: "project-api".to_string(),
                    name: "api-prod".to_string(),
                    deleted_at: None,
                    current_usage_dollars: 120.0,
                    share: 0.6,
                },
                WorkspaceUsageProject {
                    id: "project-web".to_string(),
                    name: "web-prod".to_string(),
                    deleted_at: None,
                    current_usage_dollars: 60.0,
                    share: 0.3,
                },
                WorkspaceUsageProject {
                    id: "project-old".to_string(),
                    name: "old-worker".to_string(),
                    deleted_at: Some("2026-07-04T12:00:00Z".to_string()),
                    current_usage_dollars: 20.0,
                    share: 0.1,
                },
            ],
        }
    }

    fn sample_project_summary() -> ProjectUsageSummary {
        ProjectUsageSummary {
            workspace: SummaryWorkspace {
                id: "workspace-id".to_string(),
                name: "Acme".to_string(),
            },
            project: ProjectUsageProject {
                id: "project-api".to_string(),
                name: "api".to_string(),
                deleted_at: None,
            },
            billing_period: BillingPeriod {
                start: "2026-07-01T00:00:00Z".to_string(),
                end: "2026-08-01T00:00:00Z".to_string(),
            },
            period: "current".to_string(),
            current_usage_dollars: 28.42,
            services: vec![ProjectUsageService {
                id: "service-api".to_string(),
                name: "api-server".to_string(),
                deleted_at: None,
                cpu_dollars: 8.4,
                memory_dollars: 12.71,
                egress_dollars: 4.83,
                volume_dollars: 2.48,
                backup_dollars: 0.0,
                total_dollars: 28.42,
            }],
        }
    }
}
