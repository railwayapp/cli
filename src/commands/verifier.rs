//! Create, run, and read verifiers.
//!
//! The GraphQL fields live on Backboard's internal schema
//! (`/graphql/internal`). They are not on the public `/graphql/v2` graph.

use std::time::Duration;

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;

use crate::controllers::agent_bootstrap::internal_url;
use crate::controllers::environment::get_matched_environment;
use crate::controllers::project::{
    get_project, resolve_project_id_or_name, resolve_service_context,
};

use super::*;

/// Create, run, and read verifiers
///
/// A verifier is a claim about one deployment: a plain-language intent plus a
/// check Railway runs until the claim resolves. Checks start once the
/// deployment is live.
#[derive(Parser)]
#[command(subcommand_required = true, arg_required_else_help = true)]
#[clap(
    after_help = "Examples:\n\n  railway verifier list\n  railway verifier list --status active --json\n  railway verifier get <id>\n  railway verifier run <id>\n  railway verifier create checkout-200 --intent \"checkout returns 200\" --kind http --config '{\"path\":\"/checkout\",\"expectStatus\":200}'\n  railway verifier create error-budget --intent \"error logs stay under 5\" --kind log --deployment <id> --config '{\"match\":\"error\",\"max\":5}'\n\nKinds:\n  http    { method?, path?, expectStatus?, bodyContains?, timeoutMs?, private? }\n  script  { source, timeoutMs?, env? }\n  log     { match, matchType?, max?, compareToPrevious?, windowSeconds? }\n\nVM revisions are not supported yet. The API returns at most 200 verifiers, newest first."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Project ID or name (defaults to the linked project)
    #[clap(short = 'p', long, global = true, value_name = "PROJECT")]
    project: Option<String>,

    /// Environment ID or name (defaults to the linked environment)
    #[clap(short, long, global = true)]
    environment: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// List verifiers for a project, newest first
    #[clap(visible_alias = "ls")]
    List(ListArgs),

    /// Show a verifier and its recent runs
    Get(GetArgs),

    /// Bind a verifier to a deployment and start checking it
    Create(CreateArgs),

    /// Check an active verifier now instead of at its next round
    Run(RunArgs),
}

#[derive(Parser)]
struct ListArgs {
    /// Only verifiers for this deployment
    #[clap(long)]
    deployment: Option<String>,

    /// Filter by status
    #[clap(long, value_enum)]
    status: Option<StatusArg>,

    /// Include verifiers from every environment in the project
    #[clap(long, conflicts_with = "environment")]
    all: bool,
}

#[derive(Parser)]
struct GetArgs {
    /// Verifier ID
    id: String,
}

#[derive(Parser)]
struct CreateArgs {
    /// Short slug, for example checkout-discount-200
    name: String,

    /// One plain sentence of what is now true
    #[clap(long)]
    intent: String,

    /// Check kind
    #[clap(long, value_enum)]
    kind: KindArg,

    /// Deployment the claim is about (defaults to the linked service's latest deployment)
    #[clap(long)]
    deployment: Option<String>,

    /// Service name or ID, used when --deployment is omitted
    #[clap(short, long)]
    service: Option<String>,

    /// Kind-specific config, as JSON or @path
    #[clap(long, value_name = "JSON")]
    config: Option<String>,

    /// How long checks must keep passing, in seconds (default 0: the first pass)
    #[clap(long)]
    hold_seconds: Option<u64>,

    /// Resolve by this time (RFC3339, or a duration like 30m, 1h, 7d)
    #[clap(long, conflicts_with = "deadline_seconds", value_name = "WHEN")]
    deadline: Option<String>,

    /// UNDECIDABLE if unresolved after this many seconds (default 1h, max 7d)
    #[clap(long, conflicts_with = "deadline")]
    deadline_seconds: Option<u64>,

    /// An earlier verifier this claim carries forward
    #[clap(long)]
    cloned_from: Option<String>,
}

#[derive(Parser)]
struct RunArgs {
    /// Verifier ID
    id: String,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum StatusArg {
    Active,
    Verified,
    Failed,
    Undecidable,
    Expired,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum KindArg {
    Http,
    Script,
    Log,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifierOut {
    id: String,
    name: String,
    intent: String,
    kind: String,
    status: String,
    project_id: String,
    environment_id: String,
    workload_kind: String,
    workload_id: String,
    revision_id: String,
    cloned_from_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<Value>,
    hold_seconds: i64,
    held_seconds: i64,
    deadline: DateTime<Utc>,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runs: Option<Vec<RunOut>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunOut {
    id: String,
    verdict: String,
    reason: String,
    evidence: Value,
    started_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
}

pub async fn command(args: Args) -> Result<()> {
    let Args {
        command,
        project,
        environment,
        json,
    } = args;

    match command {
        Commands::List(list_args) => list_command(project, environment, list_args, json).await,
        Commands::Get(get_args) => get_command(get_args, json).await,
        Commands::Create(create_args) => {
            create_command(project, environment, create_args, json).await
        }
        Commands::Run(run_args) => run_command(run_args, json).await,
    }
}

async fn list_command(
    project: Option<String>,
    environment: Option<String>,
    args: ListArgs,
    json: bool,
) -> Result<()> {
    let scope = resolve_scope(project, environment, args.all).await?;
    let url = internal_url(&scope.configs.get_backboard());
    let response = post_graphql_skip_none::<queries::Verifiers, _>(
        &scope.client,
        &url,
        queries::verifiers::Variables {
            project_id: scope.project_id.clone(),
            revision_id: args.deployment,
            status: args.status.map(status_var),
        },
    )
    .await?;

    let total = response.verifiers.len();
    let verifiers: Vec<_> = response
        .verifiers
        .into_iter()
        .filter(|verifier| {
            scope
                .environment_id
                .as_ref()
                .is_none_or(|environment_id| verifier.environment_id == *environment_id)
        })
        .map(view_from_list)
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&verifiers)?);
        return Ok(());
    }

    if verifiers.is_empty() {
        if scope.environment_id.is_some() && total > 0 {
            println!(
                "No verifiers found in this environment. Pass --all to include every environment."
            );
        } else {
            println!("No verifiers found");
        }
        return Ok(());
    }

    if total == 200 {
        eprintln!(
            "{}",
            "Showing the 200 newest verifiers. Narrow with --deployment or --status.".dimmed()
        );
    }
    for verifier in &verifiers {
        print_verifier(verifier, false);
        println!();
    }
    Ok(())
}

async fn get_command(args: GetArgs, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let response = post_graphql::<queries::Verifier, _>(
        &client,
        internal_url(&configs.get_backboard()),
        queries::verifier::Variables { id: args.id },
    )
    .await?;
    let verifier = view_from_get(response.verifier);
    emit(&verifier, json, true)
}

async fn create_command(
    project: Option<String>,
    environment: Option<String>,
    args: CreateArgs,
    json: bool,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let revision_id = match args.deployment {
        Some(deployment) => deployment,
        None => latest_deployment_id(project, args.service, environment).await?,
    };
    let deadline = match (args.deadline, args.deadline_seconds) {
        (Some(raw), None) => Some(parse_deadline(&raw, Utc::now())?),
        (None, Some(seconds)) => Some(deadline_after(seconds, Utc::now())?),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap rejects both deadline flags"),
    };
    let hold_seconds = args
        .hold_seconds
        .map(|seconds| i64::try_from(seconds).context("hold-seconds does not fit in an integer"))
        .transpose()?;
    let config = args.config.as_deref().map(parse_config).transpose()?;

    let response = post_graphql::<mutations::VerifierCreate, _>(
        &client,
        internal_url(&configs.get_backboard()),
        mutations::verifier_create::Variables {
            input: mutations::verifier_create::VerifierCreateInput {
                cloned_from_id: args.cloned_from,
                config,
                deadline,
                hold_seconds,
                intent: args.intent,
                kind: create_kind(args.kind),
                name: args.name,
                revision_id,
                workload_kind: mutations::verifier_create::VerifierWorkloadKind::SERVICE,
            },
        },
    )
    .await?;

    let verifier = view_from_create(response.verifier_create);
    emit(&verifier, json, false)
}

async fn run_command(args: RunArgs, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let response = post_graphql::<mutations::VerifierRunNow, _>(
        &client,
        internal_url(&configs.get_backboard()),
        mutations::verifier_run_now::Variables { id: args.id },
    )
    .await?;
    let verifier = view_from_run(response.verifier_run_now);
    if !json {
        println!(
            "Check requested. The run is recorded asynchronously; follow it with `railway verifier get {}`.",
            verifier.id
        );
        println!();
    }
    emit(&verifier, json, false)
}

struct Scope {
    client: reqwest::Client,
    configs: Configs,
    project_id: String,
    environment_id: Option<String>,
}

async fn resolve_scope(
    project_arg: Option<String>,
    environment_arg: Option<String>,
    all_environments: bool,
) -> Result<Scope> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked = if project_arg.is_none() {
        Some(configs.get_linked_project().await?)
    } else {
        configs.get_linked_project().await.ok()
    };
    let project_id = if let Some(project) = project_arg.as_deref() {
        resolve_project_id_or_name(&client, &configs, project).await?
    } else {
        linked
            .as_ref()
            .context("No project specified. Use --project or run `railway link` first")?
            .project
            .clone()
    };
    let environment_id = if all_environments {
        None
    } else if let Some(environment) = environment_arg {
        let project = get_project(&client, &configs, project_id.clone()).await?;
        Some(get_matched_environment(&project, environment)?.id)
    } else if project_arg.is_none() {
        linked.and_then(|linked| linked.environment)
    } else {
        None
    };
    Ok(Scope {
        client,
        configs,
        project_id,
        environment_id,
    })
}

async fn latest_deployment_id(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
) -> Result<String> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let response = post_graphql::<queries::LatestDeployment, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::latest_deployment::Variables {
            service_id: ctx.service_id.clone(),
            environment_id: ctx.environment_id.clone(),
        },
    )
    .await?;
    response
        .service_instance
        .latest_deployment
        .map(|deployment| deployment.id)
        .with_context(|| {
            format!(
                "Service {} has no deployment in {}. Deploy it, or pass --deployment.",
                ctx.service_name, ctx.environment_name
            )
        })
}

fn emit(verifier: &VerifierOut, json: bool, verbose: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(verifier)?);
        return Ok(());
    }
    print_verifier(verifier, verbose);
    Ok(())
}

fn print_verifier(verifier: &VerifierOut, verbose: bool) {
    let status = color_status(&verifier.status);
    println!(
        "{status}  {}  {}",
        verifier.name.bold(),
        verifier.kind.dimmed()
    );
    println!("  {}", verifier.intent);
    println!(
        "  deployment {}  held {}  deadline {}",
        verifier.revision_id,
        held_label(verifier.held_seconds, verifier.hold_seconds),
        verifier.deadline.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!("  {}", verifier.id.dimmed());
    if !verbose {
        return;
    }
    if let Some(config) = &verifier.config {
        println!(
            "  config {}",
            serde_json::to_string(config).unwrap_or_default()
        );
    }
    let Some(runs) = verifier.runs.as_deref() else {
        return;
    };
    if runs.is_empty() {
        println!("  No checks recorded yet.");
    } else {
        println!("  Recent runs:");
        for run in runs.iter().take(10) {
            println!(
                "  - {} {} {}",
                run.started_at.format("%H:%M:%S"),
                color_status(&run.verdict),
                run.reason
            );
        }
    }
    let next = next_step(&verifier.status, &verifier.id);
    if !next.is_empty() {
        println!("  {next}");
    }
}

fn color_status(status: &str) -> colored::ColoredString {
    match status {
        "VERIFIED" | "PASS" => status.green(),
        "FAILED" | "FAIL" => status.red(),
        "ACTIVE" => status.cyan(),
        "UNDECIDABLE" => status.yellow(),
        _ => status.dimmed(),
    }
}

fn held_label(held_seconds: i64, hold_seconds: i64) -> String {
    if hold_seconds <= 0 {
        return format!("{} held", duration_label(held_seconds));
    }
    format!(
        "{} of {}",
        duration_label(held_seconds),
        duration_label(hold_seconds)
    )
}

fn duration_label(seconds: i64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    let rest = seconds % 60;
    if rest == 0 {
        format!("{minutes}m")
    } else {
        format!("{minutes}m {rest}s")
    }
}

fn next_step(status: &str, id: &str) -> String {
    match status {
        "ACTIVE" => format!("Next: railway verifier get {id}"),
        "VERIFIED" => "Next: the claim held.".to_string(),
        "FAILED" => "Next: the claim failed. Read the latest run, fix the deployment, and create the verifier again.".to_string(),
        "UNDECIDABLE" => "Next: Railway could not decide. A person has to look.".to_string(),
        "EXPIRED" => format!(
            "Next: a newer deployment replaced this one. Create it again with --cloned-from {id}."
        ),
        _ => String::new(),
    }
}

fn status_var(status: StatusArg) -> queries::verifiers::VerifierStatus {
    use queries::verifiers::VerifierStatus::{ACTIVE, EXPIRED, FAILED, UNDECIDABLE, VERIFIED};
    match status {
        StatusArg::Active => ACTIVE,
        StatusArg::Expired => EXPIRED,
        StatusArg::Failed => FAILED,
        StatusArg::Undecidable => UNDECIDABLE,
        StatusArg::Verified => VERIFIED,
    }
}

fn create_kind(kind: KindArg) -> mutations::verifier_create::VerifierKind {
    use mutations::verifier_create::VerifierKind::{HTTP, LOG, SCRIPT};
    match kind {
        KindArg::Http => HTTP,
        KindArg::Log => LOG,
        KindArg::Script => SCRIPT,
    }
}

fn variant_name(value: &impl std::fmt::Debug) -> String {
    let raw = format!("{value:?}");
    raw.strip_prefix("Other(\"")
        .and_then(|rest| rest.strip_suffix("\")"))
        .unwrap_or(&raw)
        .to_string()
}

fn view_from_list(verifier: queries::verifiers::VerifiersVerifiers) -> VerifierOut {
    VerifierOut {
        id: verifier.id,
        name: verifier.name,
        intent: verifier.intent,
        kind: variant_name(&verifier.kind),
        status: variant_name(&verifier.status),
        project_id: verifier.project_id,
        environment_id: verifier.environment_id,
        workload_kind: variant_name(&verifier.workload_kind),
        workload_id: verifier.workload_id,
        revision_id: verifier.revision_id,
        cloned_from_id: verifier.cloned_from_id,
        config: None,
        hold_seconds: verifier.hold_seconds,
        held_seconds: verifier.held_seconds,
        deadline: verifier.deadline,
        created_at: verifier.created_at,
        resolved_at: verifier.resolved_at,
        runs: None,
    }
}

fn view_from_get(verifier: queries::verifier::VerifierVerifier) -> VerifierOut {
    VerifierOut {
        id: verifier.id,
        name: verifier.name,
        intent: verifier.intent,
        kind: variant_name(&verifier.kind),
        status: variant_name(&verifier.status),
        project_id: verifier.project_id,
        environment_id: verifier.environment_id,
        workload_kind: variant_name(&verifier.workload_kind),
        workload_id: verifier.workload_id,
        revision_id: verifier.revision_id,
        cloned_from_id: verifier.cloned_from_id,
        config: Some(verifier.config),
        hold_seconds: verifier.hold_seconds,
        held_seconds: verifier.held_seconds,
        deadline: verifier.deadline,
        created_at: verifier.created_at,
        resolved_at: verifier.resolved_at,
        runs: Some(
            verifier
                .runs
                .into_iter()
                .map(|run| RunOut {
                    id: run.id,
                    verdict: variant_name(&run.verdict),
                    reason: run.reason,
                    evidence: run.evidence,
                    started_at: run.started_at,
                    completed_at: run.completed_at,
                })
                .collect(),
        ),
    }
}

fn view_from_create(
    verifier: mutations::verifier_create::VerifierCreateVerifierCreate,
) -> VerifierOut {
    VerifierOut {
        id: verifier.id,
        name: verifier.name,
        intent: verifier.intent,
        kind: variant_name(&verifier.kind),
        status: variant_name(&verifier.status),
        project_id: verifier.project_id,
        environment_id: verifier.environment_id,
        workload_kind: variant_name(&verifier.workload_kind),
        workload_id: verifier.workload_id,
        revision_id: verifier.revision_id,
        cloned_from_id: verifier.cloned_from_id,
        config: Some(verifier.config),
        hold_seconds: verifier.hold_seconds,
        held_seconds: verifier.held_seconds,
        deadline: verifier.deadline,
        created_at: verifier.created_at,
        resolved_at: verifier.resolved_at,
        runs: None,
    }
}

fn view_from_run(
    verifier: mutations::verifier_run_now::VerifierRunNowVerifierRunNow,
) -> VerifierOut {
    VerifierOut {
        id: verifier.id,
        name: verifier.name,
        intent: verifier.intent,
        kind: variant_name(&verifier.kind),
        status: variant_name(&verifier.status),
        project_id: verifier.project_id,
        environment_id: verifier.environment_id,
        workload_kind: variant_name(&verifier.workload_kind),
        workload_id: verifier.workload_id,
        revision_id: verifier.revision_id,
        cloned_from_id: verifier.cloned_from_id,
        config: None,
        hold_seconds: verifier.hold_seconds,
        held_seconds: verifier.held_seconds,
        deadline: verifier.deadline,
        created_at: verifier.created_at,
        resolved_at: verifier.resolved_at,
        runs: None,
    }
}

const MAX_DEADLINE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

fn parse_deadline(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let raw = raw.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        let deadline = parsed.with_timezone(&Utc);
        return check_deadline(deadline, now);
    }
    let (amount, unit) = split_duration(raw)?;
    let seconds = match unit {
        's' => amount,
        'm' => amount.saturating_mul(60),
        'h' => amount.saturating_mul(60 * 60),
        'd' => amount.saturating_mul(24 * 60 * 60),
        _ => bail!("deadline duration must end in s, m, h, or d"),
    };
    deadline_after(seconds, now)
}

fn split_duration(raw: &str) -> Result<(u64, char)> {
    let mut chars = raw.chars();
    let unit = chars
        .next_back()
        .filter(|unit| unit.is_ascii_alphabetic())
        .context("deadline must be RFC3339 or a duration like 30m, 1h, or 7d")?;
    let amount = chars
        .as_str()
        .parse()
        .context("deadline must be RFC3339 or a duration like 30m, 1h, or 7d")?;
    Ok((amount, unit))
}

fn deadline_after(seconds: u64, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    if seconds == 0 || Duration::from_secs(seconds) > MAX_DEADLINE {
        bail!("deadline must be in the next 7 days");
    }
    Ok(now + chrono::Duration::from_std(Duration::from_secs(seconds))?)
}

fn check_deadline(deadline: DateTime<Utc>, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    if deadline <= now || deadline > now + chrono::Duration::from_std(MAX_DEADLINE)? {
        bail!("deadline must be in the next 7 days");
    }
    Ok(deadline)
}

fn parse_config(raw: &str) -> Result<Value> {
    let text = if let Some(path) = raw.strip_prefix('@') {
        std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?
    } else {
        raw.to_string()
    };
    let value: Value = serde_json::from_str(text.trim())
        .context("config must be a JSON object, or @path to one")?;
    if !value.is_object() {
        bail!("config must be a JSON object");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use graphql_client::GraphQLQuery;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap()
    }

    #[test]
    fn parses_relative_and_absolute_deadlines() {
        let deadline = parse_deadline("90m", now()).unwrap();
        assert_eq!(deadline, now() + chrono::Duration::minutes(90));
        let absolute = parse_deadline("2026-07-20T13:00:00Z", now()).unwrap();
        assert_eq!(absolute, now() + chrono::Duration::hours(1));
        assert!(parse_deadline("8d", now()).is_err());
        assert!(parse_deadline("2026-07-20T11:00:00Z", now()).is_err());
        assert!(deadline_after(0, now()).is_err());
    }

    #[test]
    fn rejects_config_that_is_not_an_object() {
        assert!(parse_config(r#"{"path":"/"}"#).is_ok());
        assert!(parse_config("[]").is_err());
        assert!(parse_config("not json").is_err());
    }

    #[test]
    fn next_step_names_the_follow_up() {
        assert!(next_step("EXPIRED", "ver_1").contains("--cloned-from ver_1"));
        assert!(next_step("ACTIVE", "ver_1").contains("verifier get ver_1"));
        assert!(next_step("UNKNOWN", "ver_1").is_empty());
    }

    #[test]
    fn create_omits_unset_input_fields() {
        let body = mutations::VerifierCreate::build_query(mutations::verifier_create::Variables {
            input: mutations::verifier_create::VerifierCreateInput {
                cloned_from_id: None,
                config: None,
                deadline: None,
                hold_seconds: None,
                intent: "returns 200".into(),
                kind: mutations::verifier_create::VerifierKind::HTTP,
                name: "checkout".into(),
                revision_id: "dep".into(),
                workload_kind: mutations::verifier_create::VerifierWorkloadKind::SERVICE,
            },
        });
        let value = serde_json::to_value(&body).unwrap();
        let input = &value["variables"]["input"];
        assert!(input.get("config").is_none());
        assert!(input.get("deadline").is_none());
        assert!(input.get("holdSeconds").is_none());
        assert!(input.get("clonedFromId").is_none());
        assert_eq!(input["kind"], "HTTP");
        assert_eq!(input["workloadKind"], "SERVICE");
        assert_eq!(input["revisionId"], "dep");
    }

    #[test]
    fn parses_subcommands() {
        assert!(Args::try_parse_from(["verifier", "list", "--status", "active", "--json"]).is_ok());
        assert!(Args::try_parse_from(["verifier", "get", "ver_1"]).is_ok());
        assert!(Args::try_parse_from(["verifier", "run", "ver_1"]).is_ok());
        assert!(
            Args::try_parse_from([
                "verifier",
                "create",
                "checkout-200",
                "--intent",
                "checkout returns 200",
                "--kind",
                "http",
                "--deployment",
                "dep_1",
                "--config",
                r#"{"path":"/checkout"}"#,
                "--hold-seconds",
                "30",
                "--deadline",
                "1h",
            ])
            .is_ok()
        );
        assert!(
            Args::try_parse_from([
                "verifier",
                "create",
                "checkout-200",
                "--intent",
                "checkout returns 200",
                "--kind",
                "http",
                "--deadline",
                "1h",
                "--deadline-seconds",
                "60",
            ])
            .is_err()
        );
        assert!(Args::try_parse_from(["verifier"]).is_err());
    }
}
