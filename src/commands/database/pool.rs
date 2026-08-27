//! The connection-pooling verb, for engines that ship a pooler companion.
//!
//! Named for the feature rather than the pooler, like its `ha` and `pitr`
//! siblings: the command tree reaches it through `Action::Pooling`, and which
//! template the pooler comes from and which image identifies it among a root's
//! edge children are read from the engine's declared pooling spec.
//!
//! What is NOT abstracted, deliberately: the tuning knobs (`POOL_MODE`,
//! `MAX_CLIENT_CONN`, ...) and the live `SHOW POOLS`/`SHOW SERVERS` probe are
//! PgBouncer's own configuration and admin protocol, and PgBouncer is the only
//! pooler any engine ships. Declaring a knob schema or a probe kind for a
//! second pooler that does not exist would be a contract with one implementer
//! and no second case to keep it honest -- so those stay concrete here, and
//! the surfaced subcommand keeps the name customers already type
//! (`railway postgres pgbouncer`).

use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use colored::Colorize;
use csv::ReaderBuilder;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::controllers::{
    config::{EnvironmentConfig, ServiceInstance, Variable, fetch_environment_config},
    database_engines::{DatabaseEngine, PoolingSpec},
    database_plugins::{self, PoolingState},
    db_stats::{parse_i64, split_sections},
    exec::exec_probe_in_container,
    project::{
        ServiceContext, find_service_instance, get_environment_instances, resolve_service_context,
    },
    regions::{
        build_multi_region_patch, merge_config, region_data_from_deployment_meta,
        validate_total_replicas,
    },
    template_apply::{
        self, ApplyKind, ApplyTemplateParams, RevertTemplateParams, stage_and_commit_patch,
    },
};

use super::{
    ResourceRef, confirm_or_bail, print_field, resolve_root, service_name_map, status_label,
};

/// Live-probe timeout -- PgBouncer's admin console usually answers instantly;
/// a service that's stopped, mid-deploy, or unreachable over SSH shouldn't
/// hang `status`.
const LIVE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Live-utilization thresholds, ported verbatim from the frontend's
/// `PgBouncerControls.tsx` (`PoolSizingRows`), which mirrors the PgBouncer
/// monitor's own warning thresholds (CLIENT_CONN_HIGH/POOL_NEAR_SATURATION at
/// 0.8, PREPARED_STMTS_NEAR_CAP at 0.9, crit at 0.95).
const CLIENT_UTIL_WARN: f64 = 0.8;
const POOL_UTIL_WARN: f64 = 0.8;
const PREPARED_UTIL_WARN: f64 = 0.9;
const UTIL_CRIT: f64 = 0.95;

/// Fallbacks for a knob that's entirely unset, ported from `PoolKnob.fallback`
/// in `PgBouncerControls.tsx` (these are the component's generic fallbacks,
/// not the template's authored defaults -- `MAX_CLIENT_CONN`/`DEFAULT_POOL_SIZE`/
/// `MAX_PREPARED_STATEMENTS` are always stamped by the template on `add`, so in
/// practice this only matters if a var was manually deleted).
const MAX_CLIENT_CONN_FALLBACK: i64 = 1000;
const DEFAULT_POOL_SIZE_FALLBACK: i64 = 20;
const MAX_PREPARED_STATEMENTS_FALLBACK: i64 = 100;

/// Manage PgBouncer connection pooling
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway postgres pgbouncer status --service postgres\n  railway postgres pgbouncer add --service postgres --pool-mode transaction\n  railway postgres pgbouncer remove --service postgres --yes\n  railway postgres pgbouncer configure --service postgres --max-client-conn 200\n  railway postgres pgbouncer scale --service postgres --replicas 2\n\nAutomation notes:\n  Works against a standalone database or an HA cluster root -- if --service points at a pooler/proxy edge node, the actual database root is resolved automatically."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Show PgBouncer status
    Status,

    /// Add PgBouncer in front of the database
    Add(AddArgs),

    /// Remove PgBouncer
    Remove(RemoveArgs),

    /// Configure pool mode and connection knobs
    Configure(ConfigureArgs),

    /// Scale PgBouncer replicas
    Scale(ScaleArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum PoolMode {
    Transaction,
    Session,
    Statement,
}

impl PoolMode {
    fn as_var_value(self) -> &'static str {
        match self {
            Self::Transaction => "transaction",
            Self::Session => "session",
            Self::Statement => "statement",
        }
    }
}

#[derive(Parser)]
struct AddArgs {
    /// Pooling mode
    #[clap(long = "pool-mode", value_enum, default_value_t = PoolMode::Transaction)]
    pool_mode: PoolMode,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Commit the config change without triggering deploys (applies on the next deploy)
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct RemoveArgs {
    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Commit the config change without triggering deploys (applies on the next deploy)
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
#[clap(group(
    clap::ArgGroup::new("setting")
        .args(["pool_mode", "max_client_conn", "default_pool_size", "max_prepared_statements"])
        .required(true)
        .multiple(true)
))]
struct ConfigureArgs {
    /// Pooling mode
    #[clap(long = "pool-mode", value_enum)]
    pool_mode: Option<PoolMode>,

    /// Maximum client connections PgBouncer accepts
    #[clap(long = "max-client-conn", value_parser = clap::value_parser!(i64).range(1..))]
    max_client_conn: Option<i64>,

    /// Default pool size per user/database pair
    #[clap(long = "default-pool-size", value_parser = clap::value_parser!(i64).range(1..))]
    default_pool_size: Option<i64>,

    /// Maximum prepared statements per connection (0 disables; ignored outside transaction mode)
    #[clap(long = "max-prepared-statements", value_parser = clap::value_parser!(i64).range(0..))]
    max_prepared_statements: Option<i64>,

    /// Commit the config change without triggering deploys (applies on the next deploy)
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct ScaleArgs {
    /// Target replica count
    #[clap(long, value_parser = clap::value_parser!(i64).range(0..))]
    replicas: i64,

    /// Commit the config change without triggering deploys (applies on the next deploy)
    #[clap(long)]
    no_deploy: bool,
}

pub async fn command(
    engine: &'static DatabaseEngine,
    args: Args,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    // Per-engine command trees only route here for engines that ship a
    // pooler, so reaching this without one is a wiring bug, not user error.
    let pooling = engine.pooling.with_context(|| {
        format!(
            "{} has no connection pooling companion.",
            engine.display_name
        )
    })?;
    let pooling = &pooling;

    match args.command {
        Commands::Status => status(pooling, project, service, environment, json).await,
        Commands::Add(a) => add(engine, pooling, project, service, environment, json, a).await,
        Commands::Remove(a) => remove(pooling, project, service, environment, json, a).await,
        Commands::Configure(a) => configure(pooling, project, service, environment, json, a).await,
        Commands::Scale(a) => scale(pooling, project, service, environment, json, a).await,
    }
}

async fn status(
    pooling: &PoolingSpec,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    print_status_with_live(pooling, &ctx, &config, json).await
}

/// Config-only status print (no live probe) -- used right after `add`/`remove`
/// stage a change, where the deployment triggered by that change may not have
/// rolled out yet, so a live probe would just report "unavailable" noise.
fn print_status(
    pooling: &PoolingSpec,
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    json: bool,
) -> Result<()> {
    let output = build_status_output(pooling, ctx, config, None);
    render_status(&output, json)
}

/// Full status print, including the live `SHOW POOLS`/`SHOW STATS`/`SHOW
/// SERVERS` probe when PgBouncer is attached. Used by the standalone `status`
/// subcommand.
async fn print_status_with_live(
    pooling: &PoolingSpec,
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    json: bool,
) -> Result<()> {
    let root = resolve_root(ctx, config);
    let state = database_plugins::compute_pooling_state(config, &root.root_id, pooling);

    let live = if let Some(edge_id) = state.edge_service_id.as_ref() {
        Some(probe_pgbouncer_live(ctx, edge_id).await)
    } else {
        None
    };

    let output = build_status_output(pooling, ctx, config, live);
    render_status(&output, json)
}

fn build_status_output(
    pooling: &PoolingSpec,
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    live: Option<PgBouncerLiveOutput>,
) -> PgBouncerStatusOutput {
    let root = resolve_root(ctx, config);
    let state = database_plugins::compute_pooling_state(config, &root.root_id, pooling);
    let names = service_name_map(ctx);
    // Replica count comes from deploy.multiRegionConfig (what the platform
    // actually writes; `pgbouncer scale` patches it too), summed across
    // regions, with the legacy flat numReplicas as fallback.
    let replicas = state
        .edge_service_id
        .as_ref()
        .and_then(|id| config.services.get(id))
        .and_then(|s| s.deploy.as_ref())
        .and_then(|d| {
            d.multi_region_config
                .as_ref()
                .map(|mrc| {
                    mrc.values()
                        .filter_map(|region| region.as_ref().and_then(|r| r.num_replicas))
                        .sum::<i64>()
                })
                .filter(|total| *total > 0)
                .or(d.num_replicas)
        });

    PgBouncerStatusOutput {
        service: ResourceRef {
            id: ctx.service_id.clone(),
            name: ctx.service_name.clone(),
        },
        environment: ResourceRef {
            id: ctx.environment_id.clone(),
            name: ctx.environment_name.clone(),
        },
        root: ResourceRef {
            id: root.root_id.clone(),
            name: root.root_name.clone(),
        },
        attached: state.attached,
        edge: state.edge_service_id.as_ref().map(|id| ResourceRef {
            id: id.clone(),
            name: names.get(id).cloned().unwrap_or_else(|| id.clone()),
        }),
        replicas,
        pool_mode: state.pool_mode.clone(),
        max_client_conn: state.max_client_conn,
        default_pool_size: state.default_pool_size,
        max_prepared_statements: state.max_prepared_statements,
        live,
    }
}

fn render_status(output: &PgBouncerStatusOutput, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(output)?);
    } else {
        print_pgbouncer_status(output);
    }
    Ok(())
}

fn print_pgbouncer_status(output: &PgBouncerStatusOutput) {
    println!("{}", "PgBouncer".bold());
    println!();
    print_field("Service:", &output.service.name.green().bold());
    print_field("Environment:", &output.environment.name.blue().bold());
    if output.root.id != output.service.id {
        print_field("Database root:", &output.root.name);
    }
    print_field("Status:", &status_label(output.attached));

    if !output.attached {
        return;
    }
    if let Some(edge) = &output.edge {
        print_field("Edge service:", &edge.name);
    }
    print_field(
        "Pool mode:",
        &output.pool_mode.clone().unwrap_or_else(|| "-".to_string()),
    );
    print_field(
        "Max client conn:",
        &output
            .max_client_conn
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
    );
    print_field(
        "Default pool size:",
        &output
            .default_pool_size
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
    );
    print_field(
        "Max prepared stmts:",
        &output
            .max_prepared_statements
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
    );

    if let Some(live) = &output.live {
        print_live_section(live, output, output.replicas.unwrap_or(1).max(1));
    }
}

fn print_live_section(live: &PgBouncerLiveOutput, knobs: &PgBouncerStatusOutput, replicas: i64) {
    println!();
    println!("{}", "Live pool stats".bold());

    if !live.reachable {
        let reason = live
            .error
            .clone()
            .unwrap_or_else(|| "probe unavailable".to_string());
        print_field("Live probe:", &format!("unavailable ({reason})").dimmed());
        return;
    }

    let max_client_conn = knobs.max_client_conn.unwrap_or(MAX_CLIENT_CONN_FALLBACK);
    let pool_size = knobs
        .default_pool_size
        .unwrap_or(DEFAULT_POOL_SIZE_FALLBACK);
    let max_prepared = knobs
        .max_prepared_statements
        .unwrap_or(MAX_PREPARED_STATEMENTS_FALLBACK);

    let client_capacity = max_client_conn * replicas;
    let clients_in_use = live.clients_active.unwrap_or(0) + live.clients_waiting.unwrap_or(0);
    print_util_line(
        "Clients:",
        clients_in_use,
        client_capacity,
        CLIENT_UTIL_WARN,
    );

    let pool_capacity = pool_size * replicas;
    let servers_open = live.servers_active.unwrap_or(0)
        + live.servers_idle.unwrap_or(0)
        + live.servers_used.unwrap_or(0);
    print_util_line("Server pool:", servers_open, pool_capacity, POOL_UTIL_WARN);

    if max_prepared > 0 {
        print_util_line(
            "Prepared stmts:",
            live.max_prepared_statements_in_use.unwrap_or(0),
            max_prepared,
            PREPARED_UTIL_WARN,
        );
    }

    if let (Some(xacts), Some(queries)) = (live.total_transactions, live.total_queries) {
        print_field(
            "Lifetime totals:",
            &format!("{xacts} transactions, {queries} queries"),
        );
    }
}

fn print_util_line(label: &str, used: i64, capacity: i64, warn_threshold: f64) {
    if capacity <= 0 {
        print_field(label, &format!("{used} in use (no configured limit)"));
        return;
    }
    let util = used as f64 / capacity as f64;
    let free = (capacity - used).max(0);
    let line = format!(
        "{used} of {capacity} in use ({:.0}%) -- {free} free",
        util * 100.0
    );
    let colored = if util >= UTIL_CRIT {
        line.red().to_string()
    } else if util >= warn_threshold {
        line.yellow().to_string()
    } else {
        line.green().to_string()
    };
    print_field(label, &colored);
}

async fn add(
    engine: &'static DatabaseEngine,
    pooling: &PoolingSpec,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: AddArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);

    let state = database_plugins::compute_pooling_state(&config, &root.root_id, pooling);
    if state.attached {
        println!(
            "PgBouncer is already attached to {}.",
            root.root_name.bold()
        );
        return print_status(pooling, &ctx, &config, json);
    }

    let names = service_name_map(&ctx);
    let ha_state = database_plugins::compute_ha_state(&config, &root.root_id, &names, engine);
    let upstream = if ha_state.is_cluster {
        "HA cluster"
    } else {
        "Postgres database"
    };

    if !confirm_or_bail(
        &format!(
            "Add PgBouncer in front of {} ({})? Connection strings will point to PgBouncer.",
            root.root_name.yellow(),
            upstream
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let mut edge_variables = BTreeMap::new();
    edge_variables.insert(
        database_plugins::POOL_MODE_VAR.to_string(),
        args.pool_mode.as_var_value().to_string(),
    );

    let result = template_apply::apply_composable_template(
        &ctx,
        ApplyTemplateParams {
            template_code: pooling.template_code.to_string(),
            service_id: root.root_id.clone(),
            volume_instance_id: None,
            replica_count: None,
            internal_count: None,
            edge_count: None,
            edge_variables: Some(edge_variables),
            kind: ApplyKind::Stacking,
            auto_deploy: !args.no_deploy,
        },
    )
    .await
    .context("Failed to add PgBouncer")?;

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    if !json {
        let verb = if result.deployed {
            "Added and deployed"
        } else {
            "Added (deploys skipped -- applies on the next deploy)"
        };
        println!(
            "{verb} PgBouncer in front of {} in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(pooling, &ctx, &config, json)
}

async fn remove(
    pooling: &PoolingSpec,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: RemoveArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);

    let state: PoolingState =
        database_plugins::compute_pooling_state(&config, &root.root_id, pooling);
    if !state.attached {
        bail!("PgBouncer is not attached to {}.", root.root_name);
    }

    if !confirm_or_bail(
        &format!(
            "Remove PgBouncer from {}? Active PgBouncer connections will be dropped.",
            root.root_name.red()
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let result = template_apply::revert_template(
        &ctx,
        RevertTemplateParams {
            template_code: pooling.template_code.to_string(),
            root_service_id: root.root_id.clone(),
            auto_deploy: !args.no_deploy,
        },
    )
    .await
    .context("Failed to remove PgBouncer")?;

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    if !json {
        let verb = if result.deployed {
            "Removed and deployed"
        } else {
            "Removed (deploys skipped -- applies on the next deploy)"
        };
        println!(
            "{verb} PgBouncer from {} in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(pooling, &ctx, &config, json)
}

async fn configure(
    pooling: &PoolingSpec,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: ConfigureArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let state = database_plugins::compute_pooling_state(&config, &root.root_id, pooling);

    if !state.attached {
        bail!(
            "PgBouncer is not attached to {}. Run `railway postgres pgbouncer add` first.",
            root.root_name
        );
    }
    let edge_id = state
        .edge_service_id
        .clone()
        .expect("attached implies an edge service id");

    let replicas = config
        .services
        .get(&edge_id)
        .and_then(|s| s.deploy.as_ref())
        .and_then(|d| d.num_replicas)
        .unwrap_or(1);

    let effective_pool_mode = args
        .pool_mode
        .map(|m| m.as_var_value().to_string())
        .or_else(|| state.pool_mode.clone())
        .unwrap_or_else(|| "transaction".to_string());
    let effective_max_client_conn = args
        .max_client_conn
        .or(state.max_client_conn)
        .unwrap_or(MAX_CLIENT_CONN_FALLBACK);
    let effective_pool_size = args
        .default_pool_size
        .or(state.default_pool_size)
        .unwrap_or(DEFAULT_POOL_SIZE_FALLBACK);
    let effective_max_prepared = args
        .max_prepared_statements
        .or(state.max_prepared_statements)
        .unwrap_or(MAX_PREPARED_STATEMENTS_FALLBACK);

    for warning in configure_advisory_warnings(AdvisoryInputs {
        pool_mode: &effective_pool_mode,
        max_client_conn: effective_max_client_conn,
        default_pool_size: effective_pool_size,
        max_prepared_statements: effective_max_prepared,
        replicas,
    }) {
        eprintln!("{} {}", "warning:".yellow().bold(), warning);
    }

    let mut variables: BTreeMap<String, Option<Variable>> = BTreeMap::new();
    if let Some(pool_mode) = args.pool_mode {
        variables.insert(
            database_plugins::POOL_MODE_VAR.to_string(),
            Some(Variable {
                value: Some(pool_mode.as_var_value().to_string()),
                ..Variable::default()
            }),
        );
    }
    if let Some(v) = args.max_client_conn {
        variables.insert(
            database_plugins::MAX_CLIENT_CONN_VAR.to_string(),
            Some(Variable {
                value: Some(v.to_string()),
                ..Variable::default()
            }),
        );
    }
    if let Some(v) = args.default_pool_size {
        variables.insert(
            database_plugins::DEFAULT_POOL_SIZE_VAR.to_string(),
            Some(Variable {
                value: Some(v.to_string()),
                ..Variable::default()
            }),
        );
    }
    if let Some(v) = args.max_prepared_statements {
        variables.insert(
            database_plugins::MAX_PREPARED_STATEMENTS_VAR.to_string(),
            Some(Variable {
                value: Some(v.to_string()),
                ..Variable::default()
            }),
        );
    }

    let patch = EnvironmentConfig {
        services: BTreeMap::from([(
            edge_id.clone(),
            ServiceInstance {
                variables,
                ..ServiceInstance::default()
            },
        )]),
        ..EnvironmentConfig::default()
    };

    let deployed = stage_and_commit_patch(&ctx, patch, !args.no_deploy)
        .await
        .context("Failed to configure PgBouncer")?;

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    if !json {
        let verb = if deployed {
            "Configured and deployed"
        } else {
            "Configured (deploys skipped -- applies on the next deploy)"
        };
        println!(
            "{verb} PgBouncer on {} in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            ctx.project_id
        );
    }
    print_status(pooling, &ctx, &config, json)
}

/// Inputs for [`configure_advisory_warnings`], the exact set of values that
/// end up wired into the PgBouncer edge service once a `configure` call is
/// applied (whichever knobs the caller didn't pass through `ConfigureArgs`
/// keep their currently-deployed value).
struct AdvisoryInputs<'a> {
    pool_mode: &'a str,
    max_client_conn: i64,
    default_pool_size: i64,
    max_prepared_statements: i64,
    replicas: i64,
}

/// Advisory (non-blocking) misconfiguration checks, ported verbatim from the
/// frontend's `PoolSizingRows` in `PgBouncerControls.tsx`: a `MAX_CLIENT_CONN`
/// below the pool's total capacity means some pooled connections can never be
/// reached, and `MAX_PREPARED_STATEMENTS = 0` under transaction pooling breaks
/// Prisma and most ORMs. Neither blocks the mutation -- callers still apply
/// the change and just print these as warnings first.
fn configure_advisory_warnings(inputs: AdvisoryInputs) -> Vec<String> {
    let mut warnings = Vec::new();
    let replicas = inputs.replicas.max(1);
    let pool_capacity = inputs.default_pool_size * replicas;

    if inputs.max_client_conn < pool_capacity {
        warnings.push(format!(
            "MAX_CLIENT_CONN ({}) is below pool capacity ({} x {replicas} replica{} = {pool_capacity}) -- some pooled connections can't be reached.",
            inputs.max_client_conn,
            inputs.default_pool_size,
            if replicas == 1 { "" } else { "s" },
        ));
    }

    if inputs.pool_mode == "transaction" && inputs.max_prepared_statements <= 0 {
        warnings.push(
            "MAX_PREPARED_STATEMENTS is 0 in transaction mode -- this breaks Prisma and most ORMs."
                .to_string(),
        );
    }

    warnings
}

async fn scale(
    pooling: &PoolingSpec,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: ScaleArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let state = database_plugins::compute_pooling_state(&config, &root.root_id, pooling);

    if !state.attached {
        bail!(
            "PgBouncer is not attached to {}. Run `railway postgres pgbouncer add` first.",
            root.root_name
        );
    }
    let edge_id = state
        .edge_service_id
        .clone()
        .expect("attached implies an edge service id");
    let edge_name = service_name_map(&ctx)
        .get(&edge_id)
        .cloned()
        .unwrap_or_else(|| edge_id.clone());

    let environment_instances = get_environment_instances(
        &ctx.client,
        &ctx.configs,
        &ctx.project_id,
        &ctx.environment_id,
    )
    .await?;
    let instance = find_service_instance(&environment_instances, &edge_id).with_context(|| {
        format!("PgBouncer edge service \"{edge_name}\" has no instance in this environment")
    })?;

    let existing = instance
        .latest_deployment
        .as_ref()
        .and_then(|d| d.meta.as_ref())
        .map(region_data_from_deployment_meta)
        .transpose()?
        .flatten()
        .unwrap_or_else(|| Value::Object(Map::new()));

    let region_id = single_scalable_region(&existing, &edge_name)?;

    let mut new_config = Map::new();
    new_config.insert(
        region_id,
        if args.replicas == 0 {
            Value::Null
        } else {
            json!({ "numReplicas": args.replicas })
        },
    );
    let region_data = merge_config(existing, new_config);
    validate_total_replicas(&region_data)?;

    let patch = build_multi_region_patch(&edge_id, &region_data)?;
    let deployed = stage_and_commit_patch(&ctx, patch, !args.no_deploy)
        .await
        .context("Failed to scale PgBouncer")?;

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    if !json {
        let verb = if deployed {
            "Scaled and deployed"
        } else {
            "Scaled (deploys skipped -- applies on the next deploy)"
        };
        println!(
            "{verb} {} to {} replica(s) in environment {} (project {}).",
            edge_name.bold(),
            args.replicas,
            ctx.environment_name.bold(),
            ctx.project_id
        );
    }
    print_status(pooling, &ctx, &config, json)
}

/// Resolves the single region `railway postgres pgbouncer scale --replicas N`
/// should target. PgBouncer/edge nodes are single-service, plain
/// container-replica scaling -- not the multi-service HA create/delete case --
/// so a bare `--replicas N` only makes unambiguous sense when the edge is
/// currently deployed to exactly one region. Zero regions (not deployed yet)
/// or more than one (already region-scaled by hand) both need the caller to
/// use `railway scale`/`railway service scale` directly instead.
fn single_scalable_region(existing: &Value, edge_name: &str) -> Result<String> {
    let mut regions: Vec<String> = existing
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    regions.sort();

    match regions.as_slice() {
        [region] => Ok(region.clone()),
        [] => bail!(
            "\"{edge_name}\" has no active deployment yet in this environment -- deploy it first, then retry `railway postgres pgbouncer scale`."
        ),
        _ => bail!(
            "\"{edge_name}\" is deployed across multiple regions ({}) -- use `railway scale --service {edge_name} <REGION>=<REPLICAS>` to control replicas per region.",
            regions.join(", ")
        ),
    }
}

// --- Live probe (SHOW POOLS / SHOW STATS / SHOW SERVERS) --------------------

/// PGBouncer's admin console answers on the same port the pooler listens on
/// for regular client traffic (5432 in Railway's `postgres-with-pgbouncer`
/// template -- confirmed against `packages/backboard/src/temporal/workflows/
/// pgbouncer-monitor/activities.ts` and the frontend's
/// `usePgBouncerAdminStats.ts`, both of which connect on port 5432, not the
/// upstream PgBouncer project's own default of 6432), via the special virtual
/// `pgbouncer` database.
fn build_pgbouncer_probe_command() -> String {
    let psql = "PGHOST=localhost PGPORT=5432 PGSSLMODE=disable psql --csv -q -P pager=off -P footer=off pgbouncer";
    format!(
        r#"echo '===POOLS===';
{psql} -c "SHOW POOLS;";
echo '===STATS===';
{psql} -c "SHOW STATS;";
echo '===SERVERS===';
{psql} -c "SHOW SERVERS;""#
    )
}

#[derive(Debug, Clone, Default, PartialEq)]
struct PgBouncerProbeRaw {
    clients_active: i64,
    clients_waiting: i64,
    servers_active: i64,
    servers_idle: i64,
    servers_used: i64,
    max_prepared_statements_in_use: i64,
    total_xact_count: i64,
    total_query_count: i64,
}

/// Parse `psql --csv` output (header row + data rows) into name -> value maps,
/// so field lookups below survive PgBouncer adding/reordering columns across
/// versions (e.g. `prepared_statements` on `SHOW SERVERS` only exists on
/// PgBouncer >= 1.21).
fn parse_named_csv_rows(csv: &str) -> Vec<std::collections::HashMap<String, String>> {
    let mut reader = ReaderBuilder::new()
        .has_headers(true)
        .from_reader(csv.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .map(|h| h.iter().map(String::from).collect())
        .unwrap_or_default();
    reader
        .records()
        .filter_map(|r| r.ok())
        .map(|record| {
            headers
                .iter()
                .cloned()
                .zip(record.iter().map(String::from))
                .collect()
        })
        .collect()
}

/// Parses the combined `SHOW POOLS`/`SHOW STATS`/`SHOW SERVERS` output.
/// Mirrors `usePgBouncerAdminStats.ts`/the pgbouncer monitor's own
/// aggregation: rows for the administrative `pgbouncer` virtual database are
/// excluded from the sums, and prepared-statement usage is a max (not a sum)
/// across server connections.
fn parse_pgbouncer_probe_output(output: &str) -> PgBouncerProbeRaw {
    let sections = split_sections(output);
    let mut raw = PgBouncerProbeRaw::default();

    let is_admin_db = |row: &std::collections::HashMap<String, String>| {
        row.get("database").map(String::as_str) == Some("pgbouncer")
    };

    if let Some(csv) = sections.get("POOLS") {
        for row in parse_named_csv_rows(csv)
            .into_iter()
            .filter(|r| !is_admin_db(r))
        {
            raw.clients_active += row.get("cl_active").map(|v| parse_i64(v)).unwrap_or(0);
            raw.clients_waiting += row.get("cl_waiting").map(|v| parse_i64(v)).unwrap_or(0);
            raw.servers_active += row.get("sv_active").map(|v| parse_i64(v)).unwrap_or(0);
            raw.servers_idle += row.get("sv_idle").map(|v| parse_i64(v)).unwrap_or(0);
            raw.servers_used += row.get("sv_used").map(|v| parse_i64(v)).unwrap_or(0);
        }
    }

    if let Some(csv) = sections.get("STATS") {
        for row in parse_named_csv_rows(csv)
            .into_iter()
            .filter(|r| !is_admin_db(r))
        {
            raw.total_xact_count += row
                .get("total_xact_count")
                .map(|v| parse_i64(v))
                .unwrap_or(0);
            raw.total_query_count += row
                .get("total_query_count")
                .map(|v| parse_i64(v))
                .unwrap_or(0);
        }
    }

    if let Some(csv) = sections.get("SERVERS") {
        for row in parse_named_csv_rows(csv)
            .into_iter()
            .filter(|r| !is_admin_db(r))
        {
            let prepared = row
                .get("prepared_statements")
                .map(|v| parse_i64(v))
                .unwrap_or(0);
            raw.max_prepared_statements_in_use = raw.max_prepared_statements_in_use.max(prepared);
        }
    }

    raw
}

async fn probe_pgbouncer_live(ctx: &ServiceContext, edge_service_id: &str) -> PgBouncerLiveOutput {
    match probe_pgbouncer_live_inner(ctx, edge_service_id).await {
        Ok(raw) => PgBouncerLiveOutput::from_raw(raw),
        Err(err) => PgBouncerLiveOutput::unavailable(format!("{err:#}")),
    }
}

async fn probe_pgbouncer_live_inner(
    ctx: &ServiceContext,
    edge_service_id: &str,
) -> Result<PgBouncerProbeRaw> {
    let environment_instances = get_environment_instances(
        &ctx.client,
        &ctx.configs,
        &ctx.project_id,
        &ctx.environment_id,
    )
    .await?;
    let instance = find_service_instance(&environment_instances, edge_service_id)
        .context("PgBouncer edge service has no instance in this environment")?;
    let instance_id = instance.id.clone();

    let command = build_pgbouncer_probe_command();
    let output = exec_probe_in_container(&instance_id, &command, LIVE_PROBE_TIMEOUT)
        .await
        .context("Probing PgBouncer's admin console failed")?;

    Ok(parse_pgbouncer_probe_output(&output))
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct PgBouncerLiveOutput {
    reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clients_active: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clients_waiting: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    servers_active: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    servers_idle: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    servers_used: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_prepared_statements_in_use: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_transactions: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_queries: Option<i64>,
}

impl PgBouncerLiveOutput {
    fn unavailable(error: String) -> Self {
        Self {
            reachable: false,
            error: Some(error),
            ..Self::default()
        }
    }

    fn from_raw(raw: PgBouncerProbeRaw) -> Self {
        Self {
            reachable: true,
            error: None,
            clients_active: Some(raw.clients_active),
            clients_waiting: Some(raw.clients_waiting),
            servers_active: Some(raw.servers_active),
            servers_idle: Some(raw.servers_idle),
            servers_used: Some(raw.servers_used),
            max_prepared_statements_in_use: Some(raw.max_prepared_statements_in_use),
            total_transactions: Some(raw.total_xact_count),
            total_queries: Some(raw.total_query_count),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PgBouncerStatusOutput {
    service: ResourceRef,
    environment: ResourceRef,
    root: ResourceRef,
    attached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    edge: Option<ResourceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replicas: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pool_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_client_conn: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_pool_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_prepared_statements: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live: Option<PgBouncerLiveOutput>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_top_level_verbs() {
        assert!(matches!(
            Args::parse_from(["pgbouncer", "status"]).command,
            Commands::Status
        ));
        assert!(matches!(
            Args::parse_from(["pgbouncer", "add"]).command,
            Commands::Add(_)
        ));
        assert!(matches!(
            Args::parse_from(["pgbouncer", "remove", "--yes"]).command,
            Commands::Remove(RemoveArgs {
                yes: true,
                no_deploy: false
            })
        ));
    }

    #[test]
    fn add_defaults_to_transaction_pool_mode() {
        let args = Args::parse_from(["pgbouncer", "add"]);
        let Commands::Add(add) = args.command else {
            panic!("expected add");
        };
        assert_eq!(add.pool_mode, PoolMode::Transaction);
    }

    #[test]
    fn add_accepts_explicit_pool_mode() {
        let args = Args::parse_from(["pgbouncer", "add", "--pool-mode", "session"]);
        let Commands::Add(add) = args.command else {
            panic!("expected add");
        };
        assert_eq!(add.pool_mode, PoolMode::Session);
    }

    #[test]
    fn configure_requires_at_least_one_setting() {
        assert!(Args::try_parse_from(["pgbouncer", "configure"]).is_err());
        let args = Args::parse_from(["pgbouncer", "configure", "--max-client-conn", "200"]);
        let Commands::Configure(configure) = args.command else {
            panic!("expected configure");
        };
        assert_eq!(configure.max_client_conn, Some(200));
        assert!(!configure.no_deploy);
    }

    #[test]
    fn configure_accepts_multiple_settings_and_no_deploy() {
        let args = Args::parse_from([
            "pgbouncer",
            "configure",
            "--pool-mode",
            "session",
            "--default-pool-size",
            "30",
            "--max-prepared-statements",
            "0",
            "--no-deploy",
        ]);
        let Commands::Configure(configure) = args.command else {
            panic!("expected configure");
        };
        assert_eq!(configure.pool_mode, Some(PoolMode::Session));
        assert_eq!(configure.default_pool_size, Some(30));
        assert_eq!(configure.max_prepared_statements, Some(0));
        assert!(configure.no_deploy);
    }

    #[test]
    fn scale_requires_replicas() {
        assert!(Args::try_parse_from(["pgbouncer", "scale"]).is_err());
        let args = Args::parse_from(["pgbouncer", "scale", "--replicas", "2"]);
        assert!(matches!(
            args.command,
            Commands::Scale(ScaleArgs {
                replicas: 2,
                no_deploy: false
            })
        ));
    }

    #[test]
    fn scale_accepts_no_deploy() {
        let args = Args::parse_from(["pgbouncer", "scale", "--replicas", "0", "--no-deploy"]);
        assert!(matches!(
            args.command,
            Commands::Scale(ScaleArgs {
                replicas: 0,
                no_deploy: true
            })
        ));
    }

    #[test]
    fn configure_rejects_nonpositive_knobs_at_parse_time() {
        assert!(Args::try_parse_from(["pgbouncer", "configure", "--max-client-conn=0"]).is_err());
        assert!(Args::try_parse_from(["pgbouncer", "configure", "--max-client-conn=-5"]).is_err());
        assert!(Args::try_parse_from(["pgbouncer", "configure", "--default-pool-size=0"]).is_err());
        assert!(
            Args::try_parse_from(["pgbouncer", "configure", "--max-prepared-statements=-1"])
                .is_err()
        );
        // Zero prepared statements is a legal (if warned-about) configuration.
        assert!(
            Args::try_parse_from(["pgbouncer", "configure", "--max-prepared-statements=0"]).is_ok()
        );
    }

    #[test]
    fn scale_rejects_negative_replicas_at_parse_time() {
        assert!(Args::try_parse_from(["pgbouncer", "scale", "--replicas=-1"]).is_err());
        assert!(Args::try_parse_from(["pgbouncer", "scale", "--replicas=0"]).is_ok());
    }

    #[test]
    fn pool_mode_var_values_are_lowercase() {
        assert_eq!(PoolMode::Transaction.as_var_value(), "transaction");
        assert_eq!(PoolMode::Session.as_var_value(), "session");
        assert_eq!(PoolMode::Statement.as_var_value(), "statement");
    }

    #[test]
    fn parse_named_csv_rows_handles_quoted_fields() {
        let rows = parse_named_csv_rows("b,a\n\"x,y\",1\n");
        assert_eq!(rows[0]["b"], "x,y");
        assert_eq!(rows[0]["a"], "1");
    }

    #[test]
    fn parse_pgbouncer_probe_output_tolerates_missing_prepared_column() {
        let output = "===POOLS===\ndatabase,cl_active,cl_waiting,sv_active,sv_idle,sv_used\nrailway,1,0,1,0,0\n===STATS===\ndatabase,total_xact_count,total_query_count\nrailway,10,20\n===SERVERS===\ndatabase,state\nrailway,idle\n";
        let raw = parse_pgbouncer_probe_output(output);
        assert_eq!(raw.max_prepared_statements_in_use, 0);
        assert_eq!(raw.clients_active, 1);
        assert_eq!(raw.total_query_count, 20);
    }

    #[test]
    fn configure_advisory_warns_below_pool_capacity() {
        let warnings = configure_advisory_warnings(AdvisoryInputs {
            pool_mode: "transaction",
            max_client_conn: 100,
            default_pool_size: 70,
            max_prepared_statements: 300,
            replicas: 2,
        });
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("MAX_CLIENT_CONN"));
        assert!(warnings[0].contains("140"));
    }

    #[test]
    fn configure_advisory_warns_zero_prepared_statements_in_transaction_mode() {
        let warnings = configure_advisory_warnings(AdvisoryInputs {
            pool_mode: "transaction",
            max_client_conn: 1000,
            default_pool_size: 20,
            max_prepared_statements: 0,
            replicas: 1,
        });
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("MAX_PREPARED_STATEMENTS"));
    }

    #[test]
    fn configure_advisory_zero_prepared_statements_ok_outside_transaction_mode() {
        let warnings = configure_advisory_warnings(AdvisoryInputs {
            pool_mode: "session",
            max_client_conn: 1000,
            default_pool_size: 20,
            max_prepared_statements: 0,
            replicas: 1,
        });
        assert!(warnings.is_empty());
    }

    #[test]
    fn configure_advisory_no_warnings_for_healthy_config() {
        let warnings = configure_advisory_warnings(AdvisoryInputs {
            pool_mode: "transaction",
            max_client_conn: 1000,
            default_pool_size: 70,
            max_prepared_statements: 300,
            replicas: 2,
        });
        assert!(warnings.is_empty());
    }

    #[test]
    fn configure_advisory_can_report_both_warnings_at_once() {
        let warnings = configure_advisory_warnings(AdvisoryInputs {
            pool_mode: "transaction",
            max_client_conn: 10,
            default_pool_size: 70,
            max_prepared_statements: 0,
            replicas: 1,
        });
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn single_scalable_region_picks_the_only_region() {
        let existing = json!({ "us-west2": { "numReplicas": 2 } });
        assert_eq!(
            single_scalable_region(&existing, "pgbouncer").unwrap(),
            "us-west2"
        );
    }

    #[test]
    fn single_scalable_region_errors_when_undeployed() {
        let existing = json!({});
        assert!(single_scalable_region(&existing, "pgbouncer").is_err());
    }

    #[test]
    fn single_scalable_region_errors_when_multi_region() {
        let existing = json!({
            "us-west2": { "numReplicas": 1 },
            "europe-west4-drams3a": { "numReplicas": 1 }
        });
        let err = single_scalable_region(&existing, "pgbouncer").unwrap_err();
        assert!(err.to_string().contains("multiple regions"));
    }

    #[test]
    fn parse_pgbouncer_probe_output_sums_pools_and_maxes_prepared_statements() {
        let output = "===POOLS===\ndatabase,cl_active,cl_waiting,sv_active,sv_idle,sv_used\npgbouncer,1,0,0,0,0\nrailway,4,1,2,3,1\n===STATS===\ndatabase,total_xact_count,total_query_count\npgbouncer,0,0\nrailway,100,500\n===SERVERS===\ndatabase,prepared_statements\nrailway,5\nrailway,12\n";
        let raw = parse_pgbouncer_probe_output(output);
        assert_eq!(raw.clients_active, 4);
        assert_eq!(raw.clients_waiting, 1);
        assert_eq!(raw.servers_active, 2);
        assert_eq!(raw.servers_idle, 3);
        assert_eq!(raw.servers_used, 1);
        assert_eq!(raw.total_xact_count, 100);
        assert_eq!(raw.total_query_count, 500);
        assert_eq!(raw.max_prepared_statements_in_use, 12);
    }

    #[test]
    fn parse_pgbouncer_probe_output_handles_missing_sections() {
        let raw = parse_pgbouncer_probe_output("");
        assert_eq!(raw, PgBouncerProbeRaw::default());
    }

    #[test]
    fn live_output_unavailable_has_no_raw_fields() {
        let live = PgBouncerLiveOutput::unavailable("connection refused".to_string());
        assert!(!live.reachable);
        assert_eq!(live.error.as_deref(), Some("connection refused"));
        assert!(live.clients_active.is_none());
    }
}
