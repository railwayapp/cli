use std::str::FromStr;

use anyhow::anyhow;
use chrono::{DateTime, Duration, Local, Utc};
use chrono_humanize::HumanTime;
use colored::ColoredString;
use croner::Cron;
use is_terminal::IsTerminal;
use serde::Serialize;

use super::*;
use crate::{
    controllers::project::{
        ProjectServiceInstanceNode, ServiceContext, find_service_instance,
        get_environment_instances, resolve_service_context,
    },
    gql::queries::deployment_instance_executions::{
        DeploymentInstanceExecutionListInput, DeploymentInstanceStatus,
    },
    util::{progress::create_spinner_if, prompt::prompt_confirm_with_default},
};

/// View recent runs of a cron service or trigger one off-schedule
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway cron list\n  railway cron list --service worker --environment production --limit 50\n  railway cron list --json\n  railway cron run --service worker --yes\n  railway cron run --yes --json\n\nAutomation notes:\n  Uses the linked service by default. Pass --service, --environment, or --project to pick another.\n  `cron run` enqueues the run and returns immediately. It can take a few minutes to show up in `cron list`.\n  Non-interactive `cron run` must pass --yes."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// List recent runs of a cron service with status, start time and duration
    #[clap(visible_alias = "ls", visible_alias = "runs")]
    List(ListArgs),

    /// Trigger an off-schedule run of a cron service now
    #[clap(visible_alias = "trigger")]
    Run(RunArgs),
}

#[derive(Parser)]
struct TargetArgs {
    /// Service name or ID (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID or name (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT")]
    project: Option<String>,
}

#[derive(Parser)]
struct ListArgs {
    #[clap(flatten)]
    target: TargetArgs,

    /// Maximum number of runs to show (default: 20, max: 1000)
    #[clap(long, default_value = "20")]
    limit: i64,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct RunArgs {
    #[clap(flatten)]
    target: TargetArgs,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    bypass: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Commands::List(args) => list(args).await,
        Commands::Run(args) => run(args).await,
    }
}

/// A cron service resolved from args plus the linked project.
struct CronService {
    ctx: ServiceContext,
    service_instance_id: String,
    cron_schedule: String,
    next_run_at: Option<DateTime<Utc>>,
}

async fn resolve_cron_service(target: TargetArgs) -> Result<CronService> {
    let ctx = resolve_service_context(target.project, target.service, target.environment).await?;
    let instances = get_environment_instances(
        &ctx.client,
        &ctx.configs,
        &ctx.project_id,
        &ctx.environment_id,
    )
    .await?;
    let instance = find_service_instance(&instances, &ctx.service_id)
        .ok_or_else(|| anyhow!("The service specified doesn't exist in the current environment"))?;

    let cron_schedule = cron_schedule_of(instance).ok_or_else(|| {
        anyhow!(
            "Service {} has no cron schedule in environment {}",
            ctx.service_name,
            ctx.environment_name
        )
    })?;

    Ok(CronService {
        service_instance_id: instance.id.clone(),
        cron_schedule,
        next_run_at: instance.next_cron_run_at,
        ctx,
    })
}

/// The schedule set on the service instance, falling back to one declared in
/// the service's config file (`railway.json` / `railway.toml`), which only
/// shows up in the latest deployment's meta.
fn cron_schedule_of(instance: &ProjectServiceInstanceNode) -> Option<String> {
    if let Some(schedule) = &instance.cron_schedule {
        return Some(schedule.clone());
    }
    let meta = instance.latest_deployment.as_ref()?.meta.as_ref()?;
    cron_schedule_from_meta(meta)
}

fn cron_schedule_from_meta(meta: &serde_json::Value) -> Option<String> {
    [
        meta.pointer("/cronSchedule"),
        meta.pointer("/fileServiceManifest/deploy/cronSchedule"),
        meta.pointer("/serviceManifest/deploy/cronSchedule"),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| value.as_str())
    .filter(|schedule| !schedule.is_empty())
    .map(str::to_owned)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunOutput {
    id: String,
    deployment_id: String,
    status: String,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    duration_seconds: Option<i64>,
    meta: serde_json::Value,
}

async fn list(args: ListArgs) -> Result<()> {
    let limit = clamp_limit(args.limit);
    let cron = resolve_cron_service(args.target).await?;
    let ctx = &cron.ctx;

    let response = post_graphql::<queries::DeploymentInstanceExecutions, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::deployment_instance_executions::Variables {
            input: DeploymentInstanceExecutionListInput {
                environment_id: ctx.environment_id.clone(),
                service_id: ctx.service_id.clone(),
            },
            first: Some(limit),
        },
    )
    .await?;

    let runs: Vec<_> = response
        .deployment_instance_executions
        .edges
        .into_iter()
        .map(|edge| edge.node)
        .collect();

    if args.json {
        let output: Vec<RunOutput> = runs
            .into_iter()
            .map(|run| RunOutput {
                duration_seconds: run
                    .completed_at
                    .map(|done| (done - run.created_at).num_seconds()),
                id: run.id,
                deployment_id: run.deployment_id,
                status: status_name(&run.status),
                created_at: run.created_at,
                completed_at: run.completed_at,
                meta: run.deployment_meta,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    println!(
        "{}",
        format!(
            "Cron runs for {} in {}",
            ctx.service_name, ctx.environment_name
        )
        .bold()
    );
    println!(
        "{}",
        describe_schedule(&cron.cron_schedule, cron.next_run_at)
    );
    println!();

    if runs.is_empty() {
        println!("No runs yet for {}", ctx.service_name);
        return Ok(());
    }

    let now = Utc::now();
    println!(
        "  {}",
        format!(
            "{:<STARTED_WIDTH$}  {:<STATUS_WIDTH$}  {:<DURATION_WIDTH$}  DEPLOYMENT",
            "STARTED", "STATUS", "DURATION"
        )
        .dimmed()
    );
    for run in &runs {
        let started: DateTime<Local> = DateTime::from(run.created_at);
        let duration = match (run.completed_at, &run.status) {
            (Some(done), _) => format_duration(done - run.created_at),
            (None, DeploymentInstanceStatus::RUNNING) => {
                format!("{} so far", format_duration(now - run.created_at))
            }
            (None, _) => "-".to_string(),
        };
        println!(
            "  {:<STARTED_WIDTH$}  {}  {:<DURATION_WIDTH$}  {}{}",
            started.format("%Y-%m-%d %H:%M:%S %Z"),
            colour_status(&run.status),
            duration,
            run.deployment_id.dimmed(),
            status_note(&run.status)
                .map(|n| format!("  {}", n.dimmed()))
                .unwrap_or_default(),
        );
    }

    if let Some(run) = runs.iter().find(|run| run.completed_at.is_some()) {
        let since = run.created_at.format("%Y-%m-%dT%H:%M:%SZ");
        // Round the end up to the next whole second so the last log line is included.
        let until = (run.completed_at.unwrap_or(run.created_at) + Duration::seconds(1))
            .format("%Y-%m-%dT%H:%M:%SZ");
        println!();
        println!(
            "{}",
            format!(
                "Logs for a run: railway logs -s {} --since {since} --until {until}",
                ctx.service_name
            )
            .dimmed()
        );
    }

    Ok(())
}

async fn run(args: RunArgs) -> Result<()> {
    let cron = resolve_cron_service(args.target).await?;
    let ctx = &cron.ctx;
    let service_name = &ctx.service_name;

    let confirmed = if args.bypass {
        true
    } else if std::io::stdout().is_terminal() {
        prompt_confirm_with_default(
            &format!(
                "Trigger an off-schedule run of {service_name} in environment {}?",
                ctx.environment_name
            ),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirmed {
        return Ok(());
    }

    let spinner = create_spinner_if(
        !args.json,
        format!("Triggering an off-schedule run of {service_name}..."),
    );

    let response = post_graphql::<mutations::DeploymentInstanceExecutionCreate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::deployment_instance_execution_create::Variables {
            input: mutations::deployment_instance_execution_create::DeploymentInstanceExecutionCreateInput {
                service_instance_id: cron.service_instance_id.clone(),
            },
        },
    )
    .await?;

    if !response.deployment_instance_execution_create {
        if let Some(spinner) = spinner {
            spinner.finish_and_clear();
        }
        bail!("Failed to trigger a run of {service_name}");
    }

    if args.json {
        println!("{}", serde_json::json!({ "success": true }));
    } else if let Some(spinner) = spinner {
        spinner.finish_with_message(format!(
            "Cron run enqueued for {}. It can take a few minutes to appear in `railway cron list`.",
            service_name.green()
        ));
    }

    Ok(())
}

const STARTED_WIDTH: usize = 24;
const STATUS_WIDTH: usize = 12;
const DURATION_WIDTH: usize = 12;

fn clamp_limit(limit: i64) -> i64 {
    if limit > 1000 {
        eprintln!(
            "{}",
            "Warning: limit cannot exceed 1000, using 1000 instead".yellow()
        );
        1000
    } else if limit < 1 {
        eprintln!(
            "{}",
            "Warning: limit must be at least 1, using 1 instead".yellow()
        );
        1
    } else {
        limit
    }
}

fn describe_schedule(schedule: &str, next_run_at: Option<DateTime<Utc>>) -> String {
    let mut line = match Cron::from_str(schedule) {
        Ok(cron) => format!("Schedule: {schedule} ({})", cron.describe().to_lowercase()),
        Err(_) => format!("Schedule: {schedule}"),
    };
    if let Some(next) = next_run_at {
        line.push_str(&format!(", next run {}", HumanTime::from(next)));
    }
    line
}

fn status_name(status: &DeploymentInstanceStatus) -> String {
    match status {
        DeploymentInstanceStatus::Other(other) => other.clone(),
        known => format!("{known:?}"),
    }
}

fn colour_status(status: &DeploymentInstanceStatus) -> ColoredString {
    let padded = format!("{:<STATUS_WIDTH$}", status_name(status));
    match status {
        DeploymentInstanceStatus::EXITED => padded.green(),
        DeploymentInstanceStatus::CRASHED => padded.red(),
        DeploymentInstanceStatus::RUNNING
        | DeploymentInstanceStatus::CREATED
        | DeploymentInstanceStatus::INITIALIZING
        | DeploymentInstanceStatus::RESTARTING => padded.blue(),
        DeploymentInstanceStatus::SKIPPED => padded.yellow(),
        _ => padded.dimmed(),
    }
}

fn status_note(status: &DeploymentInstanceStatus) -> Option<&'static str> {
    match status {
        DeploymentInstanceStatus::SKIPPED => Some("previous run still running"),
        _ => None,
    }
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.num_seconds().max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_compact() {
        assert_eq!(format_duration(Duration::seconds(0)), "0s");
        assert_eq!(format_duration(Duration::seconds(4)), "4s");
        assert_eq!(format_duration(Duration::seconds(72)), "1m 12s");
        assert_eq!(format_duration(Duration::seconds(7_500)), "2h 5m");
        assert_eq!(format_duration(Duration::seconds(-3)), "0s");
    }

    #[test]
    fn status_names_match_the_api_enum() {
        assert_eq!(status_name(&DeploymentInstanceStatus::EXITED), "EXITED");
        assert_eq!(
            status_name(&DeploymentInstanceStatus::Other("PAUSED".into())),
            "PAUSED"
        );
        assert_eq!(
            status_note(&DeploymentInstanceStatus::SKIPPED),
            Some("previous run still running")
        );
        assert_eq!(status_note(&DeploymentInstanceStatus::EXITED), None);
    }

    #[test]
    fn schedule_line_describes_and_tolerates_bad_input() {
        let line = describe_schedule("*/5 * * * *", None);
        assert!(line.starts_with("Schedule: */5 * * * * ("), "{line}");
        assert!(!line.contains("next run"));

        let line = describe_schedule("not a cron", Some(Utc::now() + Duration::minutes(3)));
        assert!(
            line.starts_with("Schedule: not a cron, next run "),
            "{line}"
        );
    }

    #[test]
    fn schedule_falls_back_to_deployment_meta() {
        let meta = serde_json::json!({
            "fileServiceManifest": { "deploy": { "cronSchedule": "0 * * * *" } }
        });
        assert_eq!(cron_schedule_from_meta(&meta).as_deref(), Some("0 * * * *"));

        let meta = serde_json::json!({ "cronSchedule": "" , "serviceManifest": {} });
        assert_eq!(cron_schedule_from_meta(&meta), None);
    }

    #[test]
    fn json_output_uses_camel_case_keys() {
        let created_at = Utc::now();
        let output = RunOutput {
            id: "exec_1".into(),
            deployment_id: "dep_1".into(),
            status: "EXITED".into(),
            created_at,
            completed_at: Some(created_at + Duration::seconds(4)),
            duration_seconds: Some(4),
            meta: serde_json::json!({}),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["deploymentId"], "dep_1");
        assert_eq!(json["durationSeconds"], 4);
        assert!(json.get("deployment_id").is_none());
    }
}
