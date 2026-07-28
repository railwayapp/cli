//! `railway postgres pgbouncer` -- PgBouncer connection pooling.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use colored::Colorize;
use serde::Serialize;

use crate::controllers::{
    config::{EnvironmentConfig, fetch_environment_config},
    postgres_plugins::{self, PgBouncerState},
    project::{ServiceContext, resolve_service_context},
    template_apply::{
        self, ApplyKind, ApplyTemplateParams, PGBOUNCER_TEMPLATE_CODE, RevertTemplateParams,
    },
};

use super::{
    ResourceRef, confirm_or_bail, not_yet_implemented, print_field, resolve_root, service_name_map,
    status_label,
};

/// Manage PgBouncer connection pooling for Postgres
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway postgres pgbouncer status --service postgres\n  railway postgres pgbouncer add --service postgres --pool-mode transaction\n  railway postgres pgbouncer remove --service postgres --yes\n  railway postgres pgbouncer configure --service postgres --max-client-conn 200\n  railway postgres pgbouncer scale --service postgres --replicas 2\n\nAutomation notes:\n  Works against a standalone Postgres or an HA cluster root -- if --service points at a PgBouncer/HAProxy edge node, the actual database root is resolved automatically."
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

    /// Stage the change without deploying it immediately
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct RemoveArgs {
    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Stage the change without deploying it immediately
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
    #[clap(long = "max-client-conn")]
    max_client_conn: Option<i64>,

    /// Default pool size per user/database pair
    #[clap(long = "default-pool-size")]
    default_pool_size: Option<i64>,

    /// Maximum prepared statements per connection (0 disables; ignored outside transaction mode)
    #[clap(long = "max-prepared-statements")]
    max_prepared_statements: Option<i64>,

    /// Stage the change without deploying it immediately
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct ScaleArgs {
    /// Target replica count
    #[clap(long)]
    replicas: i64,
}

pub async fn command(
    args: Args,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    match args.command {
        Commands::Status => status(project, service, environment, json).await,
        Commands::Add(a) => add(project, service, environment, json, a).await,
        Commands::Remove(a) => remove(project, service, environment, json, a).await,
        Commands::Configure(_) => not_yet_implemented("pgbouncer configure"),
        Commands::Scale(_) => not_yet_implemented("pgbouncer scale"),
    }
}

async fn status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    print_status(&ctx, &config, json)
}

fn print_status(ctx: &ServiceContext, config: &EnvironmentConfig, json: bool) -> Result<()> {
    let root = resolve_root(ctx, config);
    let state = postgres_plugins::compute_pgbouncer_state(config, &root.root_id);
    let names = service_name_map(ctx);

    let output = PgBouncerStatusOutput {
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
        pool_mode: state.pool_mode.clone(),
        max_client_conn: state.max_client_conn,
        default_pool_size: state.default_pool_size,
        max_prepared_statements: state.max_prepared_statements,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_pgbouncer_status(&output);
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
}

async fn add(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: AddArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);

    let state = postgres_plugins::compute_pgbouncer_state(&config, &root.root_id);
    if state.attached {
        println!(
            "PgBouncer is already attached to {}.",
            root.root_name.bold()
        );
        return print_status(&ctx, &config, json);
    }

    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);
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
        postgres_plugins::POOL_MODE_VAR.to_string(),
        args.pool_mode.as_var_value().to_string(),
    );

    let result = template_apply::apply_composable_template(
        &ctx,
        ApplyTemplateParams {
            template_code: PGBOUNCER_TEMPLATE_CODE.to_string(),
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

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    if !json {
        let verb = if result.deployed {
            "Added and deployed"
        } else {
            "Staged adding"
        };
        println!(
            "{verb} PgBouncer in front of {} in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(&ctx, &config, json)
}

async fn remove(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: RemoveArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);

    let state: PgBouncerState = postgres_plugins::compute_pgbouncer_state(&config, &root.root_id);
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
            template_code: PGBOUNCER_TEMPLATE_CODE.to_string(),
            root_service_id: root.root_id.clone(),
            auto_deploy: !args.no_deploy,
        },
    )
    .await
    .context("Failed to remove PgBouncer")?;

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    if !json {
        let verb = if result.deployed {
            "Removed and deployed"
        } else {
            "Staged removing"
        };
        println!(
            "{verb} PgBouncer from {} in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(&ctx, &config, json)
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
    pool_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_client_conn: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_pool_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_prepared_statements: Option<i64>,
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
    }

    #[test]
    fn scale_requires_replicas() {
        assert!(Args::try_parse_from(["pgbouncer", "scale"]).is_err());
        let args = Args::parse_from(["pgbouncer", "scale", "--replicas", "2"]);
        assert!(matches!(
            args.command,
            Commands::Scale(ScaleArgs { replicas: 2 })
        ));
    }
}
