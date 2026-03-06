use super::*;
use crate::{
    controllers::{
        config::{BucketInstance, EnvironmentConfig, environment::fetch_environment_config},
        environment::get_matched_environment,
        project::{ensure_project_and_environment_exist, get_project},
    },
    errors::RailwayError,
    util::{
        progress::create_spinner_if,
        prompt::{fake_select, prompt_confirm_with_default, prompt_options, prompt_text},
        two_factor::validate_two_factor_if_enabled,
    },
};
use anyhow::{Context, anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;
use std::{collections::BTreeMap, fmt::Display};

/// Manage project buckets
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Bucket name or ID
    #[clap(long, short, global = true)]
    bucket: Option<String>,

    /// Environment name or ID
    #[clap(long, short, global = true)]
    environment: Option<String>,
}

#[derive(Parser)]
struct ListArgs {
    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct CreateArgs {
    /// Optional bucket name
    name: Option<String>,

    /// Bucket region: sjc (US West), iad (US East), ams (EU West), sin (Asia Pacific)
    #[clap(long, short)]
    region: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct DeleteArgs {
    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// 2FA code for verification (required if 2FA is enabled in non-interactive mode)
    #[clap(long = "2fa-code")]
    two_factor_code: Option<String>,
}

#[derive(Parser)]
struct InfoArgs {
    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct CredentialsArgs {
    /// Reset S3 credentials
    #[clap(long)]
    reset: bool,

    /// Skip confirmation dialog when resetting credentials
    #[clap(short = 'y', long = "yes", requires = "reset")]
    yes: bool,

    /// 2FA code for verification when resetting credentials
    #[clap(long = "2fa-code", requires = "reset")]
    two_factor_code: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct RenameArgs {
    /// New bucket name
    #[clap(long, short)]
    name: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// List buckets
    #[clap(alias = "ls")]
    List(ListArgs),

    /// Create a new bucket
    #[clap(alias = "add", alias = "new")]
    Create(CreateArgs),

    /// Delete a bucket
    #[clap(alias = "remove", alias = "rm")]
    Delete(DeleteArgs),

    /// Show bucket details
    Info(InfoArgs),

    /// Show or reset S3-compatible credentials
    Credentials(CredentialsArgs),

    /// Rename a bucket
    Rename(RenameArgs),
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let environment_input = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());
    let environment = get_matched_environment(&project, environment_input)?;
    let environment_config = fetch_environment_config(&client, &configs, &environment.id, false)
        .await?
        .config;
    let is_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    let context = CommandContext {
        configs,
        client,
        project,
        environment,
        environment_config,
        is_terminal,
    };

    match args.command {
        Commands::List(sub) => list(&context, sub)?,
        Commands::Create(sub) => create(&context, sub).await?,
        Commands::Delete(sub) => delete(&context, args.bucket, sub).await?,
        Commands::Info(sub) => info(&context, args.bucket, sub).await?,
        Commands::Credentials(sub) => credentials(&context, args.bucket, sub).await?,
        Commands::Rename(sub) => rename(&context, args.bucket, sub).await?,
    }

    Ok(())
}

struct CommandContext {
    configs: Configs,
    client: reqwest::Client,
    project: queries::RailwayProject,
    environment: queries::project::ProjectProjectEnvironmentsEdgesNode,
    environment_config: EnvironmentConfig,
    is_terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BucketRecord {
    id: String,
    name: String,
    region: Option<String>,
}

impl Display for BucketRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BucketCredentials {
    endpoint: String,
    access_key_id: String,
    secret_access_key: String,
    bucket_name: String,
    region: String,
    url_style: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BucketInfo {
    id: String,
    name: String,
    environment_id: String,
    environment_name: String,
    region: String,
    size_bytes: i64,
    object_count: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BucketPatchMode {
    Commit,
    Stage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BucketRegion {
    Sjc,
    Iad,
    Ams,
    Sin,
}

impl BucketRegion {
    fn code(self) -> &'static str {
        match self {
            Self::Sjc => "sjc",
            Self::Iad => "iad",
            Self::Ams => "ams",
            Self::Sin => "sin",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sjc => "US West, California",
            Self::Iad => "US East, Virginia",
            Self::Ams => "EU West, Amsterdam",
            Self::Sin => "Asia Pacific, Singapore",
        }
    }

    fn country(self) -> &'static str {
        match self {
            Self::Sjc | Self::Iad => "US",
            Self::Ams => "NL",
            Self::Sin => "SG",
        }
    }

    fn parse(input: &str) -> Result<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "sjc" => Ok(Self::Sjc),
            "iad" => Ok(Self::Iad),
            "ams" => Ok(Self::Ams),
            "sin" => Ok(Self::Sin),
            _ => bail!("Invalid bucket region \"{input}\". Valid regions: sjc, iad, ams, sin."),
        }
    }

    fn all() -> Vec<Self> {
        vec![Self::Sjc, Self::Iad, Self::Ams, Self::Sin]
    }
}

impl Display for BucketRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.code(), self.label())
    }
}

fn list(context: &CommandContext, args: ListArgs) -> Result<()> {
    let buckets = resolve_environment_buckets(&context.project, &context.environment_config);

    if args.json {
        let output: Vec<serde_json::Value> = buckets
            .into_iter()
            .map(|bucket| serde_json::json!({ "id": bucket.id, "name": bucket.name }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if buckets.is_empty() {
        println!(
            "No buckets found in environment {}",
            context.environment.name
        );
        return Ok(());
    }

    for bucket in buckets {
        println!("{}", bucket.name);
    }

    Ok(())
}

async fn create(context: &CommandContext, args: CreateArgs) -> Result<()> {
    let json = args.json;
    let region = resolve_region(args.region, context.is_terminal, json)?;
    let spinner = create_spinner_if(context.is_terminal && !json, "Creating bucket...".into());

    let create_response = post_graphql_skip_none::<mutations::BucketCreate, _>(
        &context.client,
        context.configs.get_backboard(),
        mutations::bucket_create::Variables {
            input: mutations::bucket_create::BucketCreateInput {
                // Bucket is created at the project level; it gets deployed to the
                // environment via the patch application below.
                environment_id: None,
                name: args.name,
                project_id: context.project.id.clone(),
            },
        },
    )
    .await?;

    let bucket = create_response.bucket_create;
    let bucket_name = bucket.name.clone();
    let patch = EnvironmentConfig {
        buckets: BTreeMap::from([(
            bucket.id.clone(),
            BucketInstance {
                region: Some(region.code().to_string()),
                is_created: Some(true),
                ..BucketInstance::default()
            },
        )]),
        ..EnvironmentConfig::default()
    };

    let patch_mode = apply_bucket_patch(context, patch, Some(format!("Create bucket {bucket_name}")))
        .await
        .with_context(|| {
            let verb = if bucket_patch_mode(context.environment.unmerged_changes_count)
                == BucketPatchMode::Stage
            {
                "staged for"
            } else {
                "committed to"
            };
            format!(
                "Bucket \"{}\" was created in project \"{}\", but it could not be {} environment \"{}\".",
                bucket_name, context.project.name, verb, context.environment.name
            )
        })?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": bucket.id,
                "name": bucket.name,
                "region": region.code(),
                "staged": patch_mode == BucketPatchMode::Stage,
                "committed": patch_mode == BucketPatchMode::Commit,
            }))?
        );
    } else {
        let msg = match patch_mode {
            BucketPatchMode::Commit => format!(
                "Created bucket {} ({})",
                bucket.name.blue(),
                region.code().cyan()
            ),
            BucketPatchMode::Stage => format!(
                "Created bucket {} ({}) and staged it for {} {}",
                bucket.name.blue(),
                region.code().cyan(),
                context.environment.name.magenta().bold(),
                "(use 'railway environment edit' to commit)".dimmed()
            ),
        };
        if let Some(spinner) = spinner {
            spinner.finish_with_message(msg);
        } else {
            println!("{msg}");
        }
    }

    Ok(())
}

async fn delete(context: &CommandContext, bucket: Option<String>, args: DeleteArgs) -> Result<()> {
    let bucket = select_bucket(context, bucket)?;

    let confirmed = if args.yes {
        true
    } else if context.is_terminal {
        prompt_confirm_with_default(
            format!(
                "Are you sure you want to delete bucket \"{}\"? This will permanently delete all objects.",
                bucket.name
            )
            .as_str(),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirmed {
        if !args.json {
            println!("Deletion cancelled.");
        }
        return Ok(());
    }

    validate_two_factor_if_enabled(
        &context.client,
        &context.configs,
        context.is_terminal,
        args.two_factor_code,
    )
    .await?;

    let patch = EnvironmentConfig {
        buckets: BTreeMap::from([(
            bucket.id.clone(),
            BucketInstance {
                is_deleted: Some(true),
                ..BucketInstance::default()
            },
        )]),
        ..EnvironmentConfig::default()
    };

    let patch_mode = apply_bucket_patch(
        context,
        patch,
        Some(format!("Delete bucket {}", bucket.name)),
    )
    .await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": bucket.id,
                "name": bucket.name,
                "staged": patch_mode == BucketPatchMode::Stage,
                "committed": patch_mode == BucketPatchMode::Commit,
            }))?
        );
    } else {
        match patch_mode {
            BucketPatchMode::Commit => println!("Deleted bucket {}", bucket.name.blue()),
            BucketPatchMode::Stage => println!(
                "Staged deletion of bucket {} for {} {}",
                bucket.name.blue(),
                context.environment.name.magenta().bold(),
                "(use 'railway environment edit' to commit)".dimmed()
            ),
        }
    }

    Ok(())
}

async fn info(context: &CommandContext, bucket: Option<String>, args: InfoArgs) -> Result<()> {
    let bucket = select_bucket(context, bucket)?;
    let details = post_graphql::<queries::BucketInstanceDetails, _>(
        &context.client,
        context.configs.get_backboard(),
        queries::bucket_instance_details::Variables {
            bucket_id: bucket.id.clone(),
            environment_id: context.environment.id.clone(),
        },
    )
    .await?;

    let details = details.bucket_instance_details.ok_or_else(|| {
        anyhow!(
            "Detailed bucket stats are unavailable for bucket \"{}\" in environment \"{}\".",
            bucket.name,
            context.environment.name
        )
    })?;

    let info = BucketInfo {
        id: bucket.id.clone(),
        name: bucket.name.clone(),
        environment_id: context.environment.id.clone(),
        environment_name: context.environment.name.clone(),
        region: bucket.region.unwrap_or_else(|| "unknown".to_string()),
        size_bytes: details.size_bytes,
        object_count: details.object_count,
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": info.id,
                "name": info.name,
                "environmentId": info.environment_id,
                "environment": info.environment_name,
                "region": info.region,
                "storageBytes": info.size_bytes,
                "storage": format_bytes(info.size_bytes),
                "objects": info.object_count,
            }))?
        );
    } else {
        println!("Name:          {}", info.name);
        println!("Bucket ID:     {}", info.id);
        println!("Environment:   {}", info.environment_name);
        println!("Region:        {}", info.region);
        println!("Storage:       {}", format_bytes(info.size_bytes));
        println!("Objects:       {}", format_count(info.object_count));
    }

    Ok(())
}

async fn credentials(
    context: &CommandContext,
    bucket: Option<String>,
    args: CredentialsArgs,
) -> Result<()> {
    let bucket = select_bucket(context, bucket)?;

    if args.reset {
        let confirmed = if args.yes {
            true
        } else if context.is_terminal {
            prompt_confirm_with_default(
                "This will invalidate existing credentials. Continue?",
                false,
            )?
        } else {
            bail!(
                "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
            );
        };

        if !confirmed {
            if !args.json {
                println!("Credential reset cancelled.");
            }
            return Ok(());
        }

        validate_two_factor_if_enabled(
            &context.client,
            &context.configs,
            context.is_terminal,
            args.two_factor_code,
        )
        .await?;

        let response = post_graphql::<mutations::BucketCredentialsReset, _>(
            &context.client,
            context.configs.get_backboard(),
            mutations::bucket_credentials_reset::Variables {
                project_id: context.project.id.clone(),
                environment_id: context.environment.id.clone(),
                bucket_id: bucket.id.clone(),
            },
        )
        .await?;

        let credentials = BucketCredentials {
            endpoint: response.bucket_credentials_reset.endpoint,
            access_key_id: response.bucket_credentials_reset.access_key_id,
            secret_access_key: response.bucket_credentials_reset.secret_access_key,
            bucket_name: response.bucket_credentials_reset.bucket_name,
            region: response.bucket_credentials_reset.region,
            url_style: response.bucket_credentials_reset.url_style,
        };

        if args.json {
            print_credentials_json(&credentials)?;
        } else {
            println!("Credentials reset for {}", bucket.name);
        }

        return Ok(());
    }

    let credentials = fetch_bucket_credentials(context, &bucket.id).await?;

    if args.json {
        print_credentials_json(&credentials)?;
    } else {
        print_credentials_kv(&credentials);
    }

    Ok(())
}

async fn rename(context: &CommandContext, bucket: Option<String>, args: RenameArgs) -> Result<()> {
    let bucket = select_bucket(context, bucket)?;
    let new_name = if let Some(name) = args.name {
        name
    } else if context.is_terminal {
        prompt_text("New bucket name")?
    } else {
        bail!("Bucket name must be specified via --name in non-interactive mode.");
    };

    if context.is_terminal && !args.json {
        fake_select("New bucket name", &new_name);
    }

    let response = post_graphql::<mutations::BucketUpdate, _>(
        &context.client,
        context.configs.get_backboard(),
        mutations::bucket_update::Variables {
            id: bucket.id.clone(),
            input: mutations::bucket_update::BucketUpdateInput { name: new_name },
        },
    )
    .await?;

    let updated_bucket = response.bucket_update;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": updated_bucket.id,
                "name": updated_bucket.name,
            }))?
        );
    } else {
        println!(
            "Renamed {} -> {}",
            bucket.name.blue(),
            updated_bucket.name.purple()
        );
    }

    Ok(())
}

async fn fetch_bucket_credentials(
    context: &CommandContext,
    bucket_id: &str,
) -> Result<BucketCredentials> {
    let response = post_graphql::<queries::BucketS3Credentials, _>(
        &context.client,
        context.configs.get_backboard(),
        queries::bucket_s3_credentials::Variables {
            project_id: context.project.id.clone(),
            environment_id: context.environment.id.clone(),
            bucket_id: bucket_id.to_string(),
        },
    )
    .await?;

    let mut credentials = response.bucket_s3_credentials.into_iter();
    let Some(first) = credentials.next() else {
        bail!("No S3-compatible credentials were returned for this bucket.");
    };

    if credentials.next().is_some() {
        bail!("Expected a single S3-compatible credential set for this bucket.");
    }

    Ok(BucketCredentials {
        endpoint: first.endpoint,
        access_key_id: first.access_key_id,
        secret_access_key: first.secret_access_key,
        bucket_name: first.bucket_name,
        region: first.region,
        url_style: first.url_style,
    })
}

fn select_bucket(context: &CommandContext, bucket: Option<String>) -> Result<BucketRecord> {
    let buckets = resolve_environment_buckets(&context.project, &context.environment_config);

    if let Some(bucket_input) = bucket {
        if let Some(bucket) = buckets.iter().find(|candidate| {
            candidate.id.eq_ignore_ascii_case(&bucket_input)
                || candidate.name.eq_ignore_ascii_case(&bucket_input)
        }) {
            if context.is_terminal {
                fake_select("Bucket", &bucket.name);
            }
            return Ok(bucket.clone());
        }

        if project_has_bucket(&context.project, &bucket_input) {
            return Err(RailwayError::BucketNotInEnvironment(
                bucket_input,
                context.environment.name.clone(),
            )
            .into());
        }

        return Err(RailwayError::BucketNotFound(bucket_input).into());
    }

    if !context.is_terminal {
        bail!("Bucket must be specified via --bucket in non-interactive mode.");
    }

    if buckets.is_empty() {
        bail!(
            "No buckets found in environment {}",
            context.environment.name
        );
    }

    prompt_options("Select a bucket", buckets).context("Failed to select bucket")
}

fn resolve_environment_buckets(
    project: &queries::RailwayProject,
    environment_config: &EnvironmentConfig,
) -> Vec<BucketRecord> {
    let mut buckets: Vec<BucketRecord> = environment_config
        .buckets
        .iter()
        .filter(|(_, config)| config.is_deleted != Some(true))
        .map(|(bucket_id, config)| BucketRecord {
            id: bucket_id.clone(),
            name: project_bucket_name(project, bucket_id).unwrap_or_else(|| bucket_id.clone()),
            region: config.region.clone(),
        })
        .collect();

    buckets.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    buckets
}

fn project_bucket_name(project: &queries::RailwayProject, bucket_id: &str) -> Option<String> {
    project
        .buckets
        .edges
        .iter()
        .find(|edge| edge.node.id == bucket_id)
        .map(|edge| edge.node.name.clone())
}

fn project_has_bucket(project: &queries::RailwayProject, bucket_input: &str) -> bool {
    project.buckets.edges.iter().any(|edge| {
        edge.node.id.eq_ignore_ascii_case(bucket_input)
            || edge.node.name.eq_ignore_ascii_case(bucket_input)
    })
}

async fn apply_bucket_patch(
    context: &CommandContext,
    patch: EnvironmentConfig,
    commit_message: Option<String>,
) -> Result<BucketPatchMode> {
    let patch_mode = bucket_patch_mode(context.environment.unmerged_changes_count);

    match patch_mode {
        BucketPatchMode::Commit => {
            post_graphql::<mutations::EnvironmentPatchCommit, _>(
                &context.client,
                context.configs.get_backboard(),
                mutations::environment_patch_commit::Variables {
                    environment_id: context.environment.id.clone(),
                    patch,
                    commit_message,
                },
            )
            .await?;
        }
        BucketPatchMode::Stage => {
            post_graphql::<mutations::EnvironmentStageChanges, _>(
                &context.client,
                context.configs.get_backboard(),
                mutations::environment_stage_changes::Variables {
                    environment_id: context.environment.id.clone(),
                    input: patch,
                    merge: Some(true),
                },
            )
            .await?;
        }
    }

    Ok(patch_mode)
}

fn bucket_patch_mode(unmerged_changes_count: Option<i64>) -> BucketPatchMode {
    if unmerged_changes_count.unwrap_or_default() > 0 {
        BucketPatchMode::Stage
    } else {
        BucketPatchMode::Commit
    }
}

fn resolve_region(region: Option<String>, is_terminal: bool, json: bool) -> Result<BucketRegion> {
    match region {
        Some(region) => {
            let region = BucketRegion::parse(&region)?;
            if is_terminal && !json {
                let flag = country_emoji::flag(region.country()).unwrap_or_default();
                fake_select("Bucket region", &format!("{} {}", flag, region.label()));
            }
            Ok(region)
        }
        None if is_terminal => prompt_options("Select a bucket region", BucketRegion::all())
            .context("Failed to select bucket region"),
        None => bail!("Bucket region must be specified via --region in non-interactive mode."),
    }
}

fn print_credentials_kv(credentials: &BucketCredentials) {
    println!("AWS_ENDPOINT_URL={}", credentials.endpoint);
    println!("AWS_ACCESS_KEY_ID={}", credentials.access_key_id);
    println!("AWS_SECRET_ACCESS_KEY={}", credentials.secret_access_key);
    println!("AWS_S3_BUCKET_NAME={}", credentials.bucket_name);
    println!("AWS_DEFAULT_REGION={}", credentials.region);
    println!("AWS_S3_URL_STYLE={}", credentials.url_style);
}

fn print_credentials_json(credentials: &BucketCredentials) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "endpoint": credentials.endpoint,
            "accessKeyId": credentials.access_key_id,
            "secretAccessKey": credentials.secret_access_key,
            "bucketName": credentials.bucket_name,
            "region": credentials.region,
            "urlStyle": credentials.url_style,
        }))?
    );
    Ok(())
}

fn format_bytes(bytes: i64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_count(value: i64) -> String {
    let digits = value.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len.saturating_sub(1) / 3);

    for (i, ch) in digits.chars().enumerate() {
        if i != 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }

    out
}
