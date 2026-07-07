use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose};
use chrono::{DateTime, Utc};
use colored::Colorize;
use console::{Alignment, measure_text_width, pad_str};
use reqwest::Client;
use serde::Serialize;
use serde::ser::SerializeStruct;
use serde_json::{Value, json};

use crate::{
    LinkedProject,
    client::{GQLClient, post_graphql},
    commands::{Configs, mutations, queries},
    controllers::{
        environment::get_matched_environment,
        project::{
            ProjectEnvironmentInstances, ensure_project_and_environment_exist,
            get_environment_instances, get_project, resolve_project_id_or_name,
        },
    },
    errors::RailwayError,
};

const FUNCTION_IMAGE_PREFIX: &str = "ghcr.io/railwayapp/function-";
/// Literal the backend substitutes for sealed variable values; it must never
/// be rendered as if it were the value itself.
const SEALED_VALUE_SENTINEL: &str = "SECRET_VARIABLE_VALUE";
/// Dashboard keywords used to classify a deleted service as a database
/// (frontend `commit.tsx` DATABASE_IMAGE_KEYWORDS).
const DATABASE_IMAGE_KEYWORDS: [&str; 8] = [
    "redis",
    "mongo",
    "mysql",
    "postgres",
    "postgis",
    "mariadb",
    "clickhouse",
    "valkey",
];

#[derive(Clone)]
pub struct EnvironmentContext {
    pub client: Client,
    pub configs: Arc<Configs>,
    pub project: queries::RailwayProject,
    pub project_id: String,
    pub environment_id: String,
    pub environment_name: String,
}

#[derive(Debug, Clone)]
pub struct StagedPatch {
    pub id: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_applied_error: Option<String>,
    pub patch: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Added,
    Removed,
    Updated,
}

impl ChangeType {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Added => "+",
            Self::Removed => "-",
            Self::Updated => "~",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceKind {
    Service,
    Volume,
    Bucket,
    Group,
    SharedVariables,
    Environment,
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service => write!(f, "service"),
            Self::Volume => write!(f, "volume"),
            Self::Bucket => write!(f, "bucket"),
            Self::Group => write!(f, "group"),
            Self::SharedVariables => write!(f, "shared variables"),
            Self::Environment => write!(f, "environment"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PrettyChange {
    pub display_name: String,
    pub additional_info: Option<String>,
    pub current_value: String,
    pub new_value: String,
    pub change_type: ChangeType,
    pub path: String,
    pub is_destructive: bool,
    pub is_sealed: bool,
    pub resource_kind: ResourceKind,
    pub resource_id: Option<String>,
    pub resource_name: Option<String>,
}

// Manual impl so sealed values serialize as null: the backend's sealed-value
// sentinel must never leak into JSON output as if it were the real value.
impl Serialize for PrettyChange {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("PrettyChange", 11)?;
        state.serialize_field("displayName", &self.display_name)?;
        state.serialize_field("additionalInfo", &self.additional_info)?;
        if self.is_sealed {
            state.serialize_field("currentValue", &Option::<String>::None)?;
            state.serialize_field("newValue", &Option::<String>::None)?;
        } else {
            state.serialize_field("currentValue", &self.current_value)?;
            state.serialize_field("newValue", &self.new_value)?;
        }
        state.serialize_field("changeType", &self.change_type)?;
        state.serialize_field("path", &self.path)?;
        state.serialize_field("isDestructive", &self.is_destructive)?;
        state.serialize_field("isSealed", &self.is_sealed)?;
        state.serialize_field("resourceKind", &self.resource_kind)?;
        state.serialize_field("resourceId", &self.resource_id)?;
        state.serialize_field("resourceName", &self.resource_name)?;
        state.end()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrettyChangeGroup {
    pub resource_kind: ResourceKind,
    pub resource_id: Option<String>,
    pub resource_name: String,
    pub summary: String,
    pub changes: Vec<PrettyChange>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrettyEnvironmentChanges {
    pub total_changes: usize,
    pub groups: Vec<PrettyChangeGroup>,
}

#[derive(Debug, Clone)]
pub struct StagedChangesView {
    pub environment_id: String,
    pub environment_name: String,
    pub patch: StagedPatch,
    pub pretty: PrettyEnvironmentChanges,
    /// Live (unmerged) environment config, kept for destructive/2FA analysis.
    pub current_config: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StagedChangesOutput<'a> {
    pub environment_id: &'a str,
    pub environment_name: &'a str,
    pub patch_id: &'a str,
    pub status: &'a str,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_applied_error: &'a Option<String>,
    pub total_changes: usize,
    pub groups: &'a [PrettyChangeGroup],
    pub changes: Vec<&'a PrettyChange>,
}

pub async fn resolve_environment_context(
    project_arg: Option<String>,
    environment_arg: Option<String>,
) -> Result<EnvironmentContext> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    if project_arg.is_some() && environment_arg.is_none() {
        bail!("--environment is required when using --project");
    }

    let linked_project = if project_arg.is_none() {
        Some(configs.get_linked_project().await?)
    } else {
        None
    };

    if let Some(ref linked_project) = linked_project {
        ensure_project_and_environment_exist(&client, &configs, linked_project).await?;
    }

    let project_id = resolve_project_id(&client, &configs, project_arg, linked_project.as_ref())
        .await
        .context("Failed to resolve project")?;
    let project = get_project(&client, &configs, project_id.clone()).await?;
    let environment_input = match environment_arg {
        Some(environment) => environment,
        None => linked_project
            .as_ref()
            .context("No environment linked. Use --environment when using --project")?
            .environment_id()?
            .to_string(),
    };
    let environment = get_matched_environment(&project, environment_input)?;

    Ok(EnvironmentContext {
        client,
        configs: Arc::new(configs),
        project,
        project_id,
        environment_id: environment.id,
        environment_name: environment.name,
    })
}

async fn resolve_project_id(
    client: &Client,
    configs: &Configs,
    project_arg: Option<String>,
    linked_project: Option<&LinkedProject>,
) -> Result<String> {
    if let Some(project_arg) = project_arg {
        resolve_project_id_or_name(client, configs, &project_arg).await
    } else {
        linked_project
            .map(|linked| linked.project.clone())
            .ok_or_else(|| RailwayError::NoLinkedProject.into())
    }
}

pub async fn load_staged_changes(ctx: &EnvironmentContext) -> Result<StagedChangesView> {
    let (patch, current_config, instances) = tokio::try_join!(
        fetch_staged_patch(&ctx.client, &ctx.configs, &ctx.environment_id),
        fetch_environment_config_value(&ctx.client, &ctx.configs, &ctx.environment_id),
        get_environment_instances(
            &ctx.client,
            &ctx.configs,
            &ctx.project_id,
            &ctx.environment_id
        ),
    )?;
    let names = ResourceNames::from_context(&ctx.project, &instances);
    let pretty = prettify_patch(&patch.patch, &current_config, &names);

    Ok(StagedChangesView {
        environment_id: ctx.environment_id.clone(),
        environment_name: ctx.environment_name.clone(),
        patch,
        pretty,
        current_config,
    })
}

pub async fn fetch_staged_patch(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
) -> Result<StagedPatch> {
    let data = post_graphql::<queries::EnvironmentStagedChanges, _>(
        client,
        configs.get_backboard(),
        queries::environment_staged_changes::Variables {
            environment_id: environment_id.to_string(),
            decrypt_variables: Some(true),
        },
    )
    .await?;
    let patch = data.environment_staged_changes;

    Ok(StagedPatch {
        id: patch.id,
        status: format!("{:?}", patch.status),
        created_at: patch.created_at,
        updated_at: patch.updated_at,
        last_applied_error: patch.last_applied_error,
        patch: patch.patch,
    })
}

pub async fn fetch_environment_config_value(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
) -> Result<Value> {
    let data = post_graphql::<queries::GetEnvironmentConfig, _>(
        client,
        configs.get_backboard(),
        queries::get_environment_config::Variables {
            id: environment_id.to_string(),
            decrypt_variables: Some(true),
        },
    )
    .await?;
    Ok(data.environment.config)
}

/// Terminal outcome of waiting on a commit's apply workflow. `Pending` means
/// the commit was accepted but the apply outlived our poll budget — it is not
/// a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployWaitResult {
    Committed,
    Failed(String),
    Pending,
}

#[derive(Debug, Clone)]
pub struct DeployOutcome {
    pub workflow_id: String,
    pub wait: DeployWaitResult,
}

/// One poll's verdict while waiting for the commit workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PollDecision {
    Success,
    Failure(String),
    Waiting,
    /// workflowStatus can't see the workflow; consult the staged patch itself.
    PatchFallback,
}

const DEPLOY_POLL_INTERVAL: Duration = Duration::from_secs(5);
const DEPLOY_POLL_BUDGET: u32 = 60; // × 5s = 5 minutes
/// Polls to tolerate `NotFound` before falling back to the patch state — the
/// workflow may not be visible to workflowStatus immediately after commit.
const NOT_FOUND_GRACE_POLLS: u32 = 3;

pub async fn deploy_staged_changes(
    ctx: &EnvironmentContext,
    message: Option<String>,
    skip_deploys: Option<bool>,
) -> Result<DeployOutcome> {
    let data = post_graphql::<mutations::EnvironmentPatchCommitStaged, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::environment_patch_commit_staged::Variables {
            environment_id: ctx.environment_id.clone(),
            commit_message: message,
            skip_deploys,
        },
    )
    .await?;
    let workflow_id = data.environment_patch_commit_staged;

    let wait = wait_for_commit_workflow(ctx, &workflow_id).await;
    Ok(DeployOutcome { workflow_id, wait })
}

/// The commit mutation succeeding means the commit was accepted; from here on
/// transient poll errors and timeouts must never surface as a failed deploy.
async fn wait_for_commit_workflow(ctx: &EnvironmentContext, workflow_id: &str) -> DeployWaitResult {
    for poll in 0..DEPLOY_POLL_BUDGET {
        if poll > 0 {
            tokio::time::sleep(DEPLOY_POLL_INTERVAL).await;
        }

        let status = post_graphql::<queries::WorkflowStatus, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            queries::workflow_status::Variables {
                workflow_id: workflow_id.to_string(),
            },
        )
        .await;

        let decision = match status {
            Ok(data) => classify_workflow_poll(
                &data.workflow_status.status,
                data.workflow_status.error.as_deref(),
                poll >= NOT_FOUND_GRACE_POLLS,
            ),
            // Transient poll failure: keep waiting rather than reporting a
            // committed deploy as failed.
            Err(_) => PollDecision::Waiting,
        };

        let decision = match decision {
            PollDecision::PatchFallback => {
                match fetch_staged_patch(&ctx.client, &ctx.configs, &ctx.environment_id).await {
                    Ok(patch) => classify_patch_state(
                        &patch.id,
                        &patch.status,
                        patch.last_applied_error.as_deref(),
                    ),
                    Err(_) => PollDecision::Waiting,
                }
            }
            other => other,
        };

        match decision {
            PollDecision::Success => return DeployWaitResult::Committed,
            PollDecision::Failure(message) => {
                // The patch's lastAppliedError usually carries the richer
                // apply-time message; prefer it when present.
                let detail = match fetch_staged_patch(
                    &ctx.client,
                    &ctx.configs,
                    &ctx.environment_id,
                )
                .await
                {
                    Ok(patch) => patch
                        .last_applied_error
                        .filter(|error| !error.is_empty())
                        .unwrap_or(message),
                    Err(_) => message,
                };
                return DeployWaitResult::Failed(detail);
            }
            PollDecision::Waiting | PollDecision::PatchFallback => {}
        }
    }

    DeployWaitResult::Pending
}

fn classify_workflow_poll(
    status: &queries::workflow_status::WorkflowStatus,
    error: Option<&str>,
    past_grace: bool,
) -> PollDecision {
    use queries::workflow_status::WorkflowStatus;
    match status {
        WorkflowStatus::Complete => PollDecision::Success,
        WorkflowStatus::Error => PollDecision::Failure(
            error
                .filter(|message| !message.is_empty())
                .unwrap_or("staged changes failed to apply")
                .to_string(),
        ),
        WorkflowStatus::NotFound if past_grace => PollDecision::PatchFallback,
        WorkflowStatus::NotFound | WorkflowStatus::Running | WorkflowStatus::Other(_) => {
            PollDecision::Waiting
        }
    }
}

/// Fallback verdict from the staged patch itself. The backend has no FAILED
/// status: a failed apply reverts the patch to STAGED with lastAppliedError
/// set, and a successful one leaves either COMMITTED or a cleared (`<empty>`)
/// patch behind.
fn classify_patch_state(id: &str, status: &str, last_applied_error: Option<&str>) -> PollDecision {
    if status == "APPLYING" {
        return PollDecision::Waiting;
    }
    if id == EMPTY_PATCH_ID || status == "COMMITTED" {
        return PollDecision::Success;
    }
    match last_applied_error.filter(|error| !error.is_empty()) {
        Some(error) => PollDecision::Failure(error.to_string()),
        None => PollDecision::Waiting,
    }
}

/// Synthetic patch id the backend returns when nothing is staged.
pub const EMPTY_PATCH_ID: &str = "<empty>";

pub async fn discard_all_staged_changes(ctx: &EnvironmentContext) -> Result<StagedPatch> {
    stage_patch_value(ctx, json!({}), Some(false)).await?;
    fetch_staged_patch(&ctx.client, &ctx.configs, &ctx.environment_id).await
}

pub async fn discard_staged_change_paths(
    ctx: &EnvironmentContext,
    paths: &[String],
) -> Result<(StagedPatch, usize)> {
    discard_staged_change_paths_with(&ctx.client, &ctx.configs, &ctx.environment_id, paths).await
}

pub async fn discard_staged_change_paths_with(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    paths: &[String],
) -> Result<(StagedPatch, usize)> {
    if paths.is_empty() {
        bail!("No staged change paths provided.");
    }

    // Re-fetch immediately before the read-modify-write re-stage to keep the
    // clobber window against concurrent staging as small as possible.
    let fresh = fetch_staged_patch(client, configs, environment_id).await?;
    let mut patch = fresh.patch.clone();
    let matched_paths = match_staged_paths(&patch, paths)?;

    for path in &matched_paths {
        remove_dot_path(&mut patch, path);
    }

    stage_patch_value_with(client, configs, environment_id, patch, Some(false)).await?;
    let updated = fetch_staged_patch(client, configs, environment_id).await?;
    Ok((updated, matched_paths.len()))
}

/// Expands requested patterns against the patch's flattened paths, erroring
/// with near-miss suggestions when a pattern matches nothing.
pub fn match_staged_paths(patch: &Value, requested: &[String]) -> Result<Vec<String>> {
    let flattened_paths = flatten_value(patch).into_keys().collect::<Vec<_>>();
    let mut matched_paths = Vec::new();

    for pattern in requested {
        let matches = flattened_paths
            .iter()
            .filter(|path| paths_match(pattern, path))
            .cloned()
            .collect::<Vec<_>>();
        if matches.is_empty() {
            bail!(
                "No staged change matches path '{}'.{}",
                pattern,
                near_miss_hint(pattern, &flattened_paths)
            );
        }
        matched_paths.extend(matches);
    }
    matched_paths.sort();
    matched_paths.dedup();
    Ok(matched_paths)
}

fn near_miss_hint(pattern: &str, available: &[String]) -> String {
    let first_segment = split_escaped_path(pattern)
        .first()
        .map(|segment| segment.raw.clone())
        .unwrap_or_default();
    let mut candidates = available
        .iter()
        .filter(|path| path.starts_with(&first_segment) || path.contains(pattern.trim_matches('*')))
        .take(5)
        .cloned()
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates = available.iter().take(5).cloned().collect();
    }
    if candidates.is_empty() {
        return String::new();
    }
    format!("\nStaged paths include:\n  {}", candidates.join("\n  "))
}

async fn stage_patch_value(
    ctx: &EnvironmentContext,
    input: Value,
    merge: Option<bool>,
) -> Result<()> {
    stage_patch_value_with(&ctx.client, &ctx.configs, &ctx.environment_id, input, merge).await
}

async fn stage_patch_value_with(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    input: Value,
    merge: Option<bool>,
) -> Result<()> {
    post_graphql::<mutations::raw_config::EnvironmentStageChangesRaw, _>(
        client,
        configs.get_backboard(),
        mutations::raw_config::environment_stage_changes_raw::Variables {
            environment_id: environment_id.to_string(),
            input,
            merge,
        },
    )
    .await?;
    Ok(())
}

pub fn is_empty_patch(patch: &Value) -> bool {
    flatten_value(patch).is_empty()
}

pub fn output_json(view: &StagedChangesView) -> StagedChangesOutput<'_> {
    StagedChangesOutput {
        environment_id: &view.environment_id,
        environment_name: &view.environment_name,
        patch_id: &view.patch.id,
        status: &view.patch.status,
        created_at: view.patch.created_at,
        updated_at: view.patch.updated_at,
        last_applied_error: &view.patch.last_applied_error,
        total_changes: view.pretty.total_changes,
        groups: &view.pretty.groups,
        changes: view
            .pretty
            .groups
            .iter()
            .flat_map(|group| group.changes.iter())
            .collect(),
    }
}

pub fn render_status_text(view: &StagedChangesView, show_values: bool) -> String {
    if view.pretty.total_changes == 0 {
        return format!(
            "No staged changes for environment {}.",
            view.environment_name
        );
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{} {} to apply\n\n",
        view.pretty.total_changes,
        if view.pretty.total_changes == 1 {
            "change"
        } else {
            "changes"
        }
    ));
    out.push_str(&format!(
        "Environment: {} ({})\n",
        view.environment_name, view.environment_id
    ));
    out.push_str(&format!(
        "Patch: {} ({})\n\n",
        view.patch.id, view.patch.status
    ));

    for group in &view.pretty.groups {
        out.push_str(&render_group(group, show_values));
        out.push('\n');
    }

    if let Some(error) = &view.patch.last_applied_error {
        out.push_str(&format!("Last applied error: {error}\n"));
    }

    out
}

pub fn print_status(view: &StagedChangesView, show_values: bool) {
    if view.pretty.total_changes == 0 {
        println!(
            "No staged changes for environment {}.",
            view.environment_name.magenta().bold()
        );
        return;
    }

    println!(
        "{} {} to apply\n",
        view.pretty.total_changes.to_string().bold(),
        if view.pretty.total_changes == 1 {
            "change"
        } else {
            "changes"
        }
    );
    println!(
        "{} {}",
        "Environment".dimmed(),
        view.environment_name.magenta().bold()
    );
    println!(
        "{} {} ({})\n",
        "Patch".dimmed(),
        view.patch.id,
        view.patch.status
    );

    for group in &view.pretty.groups {
        print!("{}", render_group_colored(group, show_values));
        println!();
    }

    if let Some(error) = &view.patch.last_applied_error {
        println!("{} {}", "Last applied error:".red().bold(), error);
    }
}

fn render_group(group: &PrettyChangeGroup, show_values: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} will be updated\n", group.resource_name));
    if !group.summary.is_empty() {
        out.push_str(&format!("{}\n", group.summary));
    }
    out.push_str(&render_changes_table(&group.changes, false, show_values));
    out
}

fn render_group_colored(group: &PrettyChangeGroup, show_values: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {}\n",
        group.resource_name.blue().bold(),
        "will be updated".dimmed()
    ));
    if !group.summary.is_empty() {
        out.push_str(&format!("{}\n", group.summary.dimmed()));
    }
    out.push_str(&render_changes_table(&group.changes, true, show_values));
    out
}

fn render_changes_table(changes: &[PrettyChange], colored: bool, show_values: bool) -> String {
    let mut change_width = "Change".len();
    let mut current_width = "Current Value".len();
    let mut new_width = "New Value".len();

    let rendered = changes
        .iter()
        .map(|change| {
            (
                render_value(change, &change.current_value, show_values),
                render_value(change, &change.new_value, show_values),
            )
        })
        .collect::<Vec<_>>();

    for (change, (current_value, new_value)) in changes.iter().zip(&rendered) {
        let name = change_label(change);
        change_width = change_width.max(measure_text_width(&name) + 2);
        current_width = current_width.max(measure_text_width(current_value));
        new_width = new_width.max(measure_text_width(new_value));
    }

    change_width = change_width.clamp(18, 42);
    current_width = current_width.clamp(13, 36);
    new_width = new_width.clamp(9, 44);

    let mut out = String::new();
    let header = format!(
        "  {}  {}  {}\n",
        pad_str("Change", change_width, Alignment::Left, None),
        pad_str("Current Value", current_width, Alignment::Left, None),
        "New Value"
    );
    if colored {
        out.push_str(&header.dimmed().to_string());
    } else {
        out.push_str(&header);
    }

    for (change, (current_value, new_value)) in changes.iter().zip(&rendered) {
        let symbol = change.change_type.symbol();
        let label = change_label(change);
        let row = format!(
            "  {} {}  {}  {}\n",
            symbol,
            pad_str(
                &label,
                change_width.saturating_sub(2),
                Alignment::Left,
                None
            ),
            pad_str(
                &truncate_cell(current_value, current_width),
                current_width,
                Alignment::Left,
                None
            ),
            truncate_cell(new_value, new_width)
        );

        if colored {
            let colored_row = match change.change_type {
                ChangeType::Added => row.green(),
                ChangeType::Removed => row.red(),
                ChangeType::Updated => row.blue(),
            };
            out.push_str(&colored_row.to_string());
        } else {
            out.push_str(&row);
        }
    }

    out
}

fn truncate_cell(value: &str, width: usize) -> String {
    if measure_text_width(value) <= width {
        return value.to_string();
    }
    if width <= 1 {
        return String::new();
    }
    let mut output = String::new();
    for ch in value.chars() {
        if measure_text_width(&output) + measure_text_width(&ch.to_string()) >= width {
            break;
        }
        output.push(ch);
    }
    output.push_str("...");
    output
}

fn change_label(change: &PrettyChange) -> String {
    match &change.additional_info {
        Some(info) if !info.is_empty() => format!("{} ({})", change.display_name, info),
        _ => change.display_name.clone(),
    }
}

pub fn prettify_patch(
    patch: &Value,
    environment_config: &Value,
    names: &ResourceNames,
) -> PrettyEnvironmentChanges {
    let mut groups = Vec::new();

    collect_service_changes(patch, environment_config, names, &mut groups);
    collect_volume_changes(patch, environment_config, names, &mut groups);
    collect_bucket_changes(patch, environment_config, names, &mut groups);
    collect_group_changes(patch, environment_config, &mut groups);
    collect_shared_variable_changes(patch, environment_config, &mut groups);
    collect_private_networking_change(patch, environment_config, &mut groups);
    collect_raw_fallback_changes(patch, environment_config, names, &mut groups);

    // The backend substitutes a sentinel for values it refuses to decrypt
    // (sealed variables, missing permission); mark those sealed so no output
    // path renders the sentinel as a value.
    for group in &mut groups {
        for change in &mut group.changes {
            if change.current_value == SEALED_VALUE_SENTINEL
                || change.new_value == SEALED_VALUE_SENTINEL
            {
                change.is_sealed = true;
            }
        }
        group.summary = group_summary(&group.changes);
    }

    let total_changes = groups.iter().map(|group| group.changes.len()).sum();
    PrettyEnvironmentChanges {
        total_changes,
        groups,
    }
}

/// Restricts a view to changes matching any of the given path patterns
/// (same matcher as discard). Errors with suggestions if a pattern matches
/// nothing.
pub fn filter_view_by_paths(
    view: &StagedChangesView,
    patterns: &[String],
) -> Result<StagedChangesView> {
    if patterns.is_empty() {
        return Ok(view.clone());
    }
    // Validate patterns against the raw patch so near-miss hints list real paths.
    let matched_paths = match_staged_paths(&view.patch.patch, patterns)?;

    let mut groups = Vec::new();
    for group in &view.pretty.groups {
        let changes = group
            .changes
            .iter()
            .filter(|change| {
                patterns
                    .iter()
                    .any(|pattern| paths_match(pattern, &change.path))
                    || matched_paths
                        .iter()
                        .any(|raw_path| change_represents_raw_path(&change.path, raw_path))
            })
            .cloned()
            .collect::<Vec<_>>();
        if !changes.is_empty() {
            groups.push(PrettyChangeGroup {
                resource_kind: group.resource_kind,
                resource_id: group.resource_id.clone(),
                resource_name: group.resource_name.clone(),
                summary: group_summary(&changes),
                changes,
            });
        }
    }

    let total_changes = groups.iter().map(|group| group.changes.len()).sum();
    Ok(StagedChangesView {
        environment_id: view.environment_id.clone(),
        environment_name: view.environment_name.clone(),
        patch: view.patch.clone(),
        pretty: PrettyEnvironmentChanges {
            total_changes,
            groups,
        },
        current_config: view.current_config.clone(),
    })
}

fn is_variable_like(change: &PrettyChange) -> bool {
    matches!(
        change.additional_info.as_deref(),
        Some("Variable" | "Variable generator" | "Shared Variable")
    )
}

/// Human-table rendering of a possibly secret value. Sealed values are never
/// shown; plain variable values are masked unless the user opted in.
fn render_value(change: &PrettyChange, value: &str, show_values: bool) -> String {
    if change.is_sealed {
        if value.is_empty() {
            return String::new();
        }
        return "«sealed»".into();
    }
    if !show_values && is_variable_like(change) && !value.is_empty() {
        return mask_value(value);
    }
    value.to_string()
}

fn mask_value(value: &str) -> String {
    const VISIBLE_PREFIX: usize = 4;
    if value.chars().count() <= VISIBLE_PREFIX {
        return "••••".into();
    }
    let prefix: String = value.chars().take(VISIBLE_PREFIX).collect();
    format!("{prefix}…")
}

/// Copy of the view with variable values masked, for surfaces whose JSON
/// output should default to hiding secrets (the MCP status tool).
pub fn mask_view_values(view: &StagedChangesView) -> StagedChangesView {
    let mut masked = view.clone();
    for group in &mut masked.pretty.groups {
        for change in &mut group.changes {
            if change.is_sealed || !is_variable_like(change) {
                continue;
            }
            if !change.current_value.is_empty() {
                change.current_value = mask_value(&change.current_value);
            }
            if !change.new_value.is_empty() {
                change.new_value = mask_value(&change.new_value);
            }
        }
    }
    masked
}

fn collect_raw_fallback_changes(
    patch: &Value,
    environment_config: &Value,
    names: &ResourceNames,
    groups: &mut Vec<PrettyChangeGroup>,
) {
    let raw_patch = flatten_value(patch);
    if raw_patch.is_empty() {
        return;
    }

    let raw_config = flatten_value(environment_config);
    for (path, patch_value) in raw_patch {
        if raw_path_represented(&path, groups) {
            continue;
        }

        let (kind, resource_id, resource_name, display_name) = raw_fallback_context(&path, names);
        let mut change = pretty_change(
            display_name,
            raw_config.get(&path),
            Some(&patch_value),
            change_type(raw_config.get(&path), &patch_value),
            &path,
            false,
            kind,
            resource_id.as_deref(),
            Some(&resource_name),
        );
        if change.current_value == SEALED_VALUE_SENTINEL
            || change.new_value == SEALED_VALUE_SENTINEL
        {
            change.is_sealed = true;
        }
        push_change_to_group(groups, kind, resource_id, resource_name, change);
    }
}

fn raw_path_represented(path: &str, groups: &[PrettyChangeGroup]) -> bool {
    groups
        .iter()
        .flat_map(|group| group.changes.iter())
        .any(|change| change_represents_raw_path(&change.path, path))
}

fn change_represents_raw_path(change_path: &str, raw_path: &str) -> bool {
    change_path == raw_path
        || (change_path.ends_with(".source.autoUpdates")
            && raw_path.strip_prefix(change_path) == Some(".schedule"))
}

fn raw_fallback_context(
    path: &str,
    names: &ResourceNames,
) -> (ResourceKind, Option<String>, String, DisplayName) {
    let segments = split_escaped_path(path);
    let key = |idx: usize| segments.get(idx).map(|segment| segment.key.as_str());
    let display_name = DisplayName {
        display_name: segments
            .last()
            .map(|segment| camel_case_to_title(&segment.key))
            .unwrap_or_else(|| path.into()),
        additional_info: Some("Raw change".into()),
    };

    match (key(0), key(1)) {
        (Some("services"), Some(service_id)) => (
            ResourceKind::Service,
            Some(service_id.into()),
            names.service_name(service_id).to_string(),
            display_name,
        ),
        (Some("volumes"), Some(volume_id)) => (
            ResourceKind::Volume,
            Some(volume_id.into()),
            names.volume_name(volume_id).to_string(),
            display_name,
        ),
        (Some("buckets"), Some(bucket_id)) => (
            ResourceKind::Bucket,
            Some(bucket_id.into()),
            names.bucket_name(bucket_id).to_string(),
            display_name,
        ),
        (Some("groups"), Some(group_id)) => (
            ResourceKind::Group,
            Some(group_id.into()),
            group_id.into(),
            display_name,
        ),
        (Some("sharedVariables"), _) => (
            ResourceKind::SharedVariables,
            None,
            "Shared Variables".into(),
            display_name,
        ),
        _ => (
            ResourceKind::Environment,
            None,
            "Environment".into(),
            display_name,
        ),
    }
}

fn push_change_to_group(
    groups: &mut Vec<PrettyChangeGroup>,
    kind: ResourceKind,
    resource_id: Option<String>,
    resource_name: String,
    change: PrettyChange,
) {
    if let Some(group) = groups
        .iter_mut()
        .find(|group| group.resource_kind == kind && group.resource_id == resource_id)
    {
        group.changes.push(change);
        return;
    }

    groups.push(change_group(kind, resource_id, resource_name, vec![change]));
}

fn collect_service_changes(
    patch: &Value,
    environment_config: &Value,
    names: &ResourceNames,
    groups: &mut Vec<PrettyChangeGroup>,
) {
    let Some(services) = patch.get("services").and_then(Value::as_object) else {
        return;
    };

    let mut entries = services.iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    for (service_id, service_patch) in entries {
        if service_patch.is_null() {
            continue;
        }
        let service_config = environment_config
            .get("services")
            .and_then(|services| services.get(service_id))
            .unwrap_or(&Value::Null);
        let flattened_patch = flatten_value(service_patch);
        let flattened_config = flatten_value(service_config);
        let mut patch_entries = flattened_patch.into_iter().collect::<Vec<_>>();
        patch_entries.sort_by(|(left, _), (right, _)| {
            service_sort_key(left)
                .cmp(&service_sort_key(right))
                .then_with(|| left.cmp(right))
        });

        let mut changes = Vec::new();
        for (path, patch_value) in patch_entries {
            let full_path = format!("services.{service_id}.{path}");
            let name_info = get_change_display_name(&path);
            let original_value = flattened_config.get(&path);
            let change = service_change(
                &path,
                &full_path,
                patch_value,
                original_value,
                &flattened_config,
                service_patch,
                service_config,
                name_info,
                service_id,
                names.service_name(service_id),
            );
            changes.push(change);
        }

        if !changes.is_empty() {
            groups.push(change_group(
                ResourceKind::Service,
                Some(service_id.clone()),
                names.service_name(service_id).to_string(),
                changes,
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn service_change(
    path: &str,
    full_path: &str,
    patch_value: Value,
    original_value: Option<&Value>,
    flattened_config: &BTreeMap<String, Value>,
    service_patch: &Value,
    service_config: &Value,
    name_info: DisplayName,
    service_id: &str,
    service_name: &str,
) -> PrettyChange {
    let is_variable_change = path.starts_with("variables.");
    let is_volume_mounts_change = path.starts_with("volumeMounts.");
    let is_tcp_proxy_change = path.starts_with("networking.tcpProxies.");
    let is_service_domain_change = path.starts_with("networking.serviceDomains.");
    let is_multi_region_config_change = path.starts_with("deploy.multiRegionConfig.");
    let is_cpu_change = path.starts_with("deploy.limitOverride.containers.cpu");
    let is_memory_change = path.starts_with("deploy.limitOverride.containers.memoryBytes");
    let is_auto_updates_schedule = path == "source.autoUpdates.schedule";
    let is_service_deleted = path.ends_with("isDeleted") && patch_value == Value::Bool(true);
    let is_service_created = path.ends_with("isCreated") && patch_value == Value::Bool(true);

    if is_variable_change {
        let variable_original = if patch_value.is_null() {
            flattened_config.get(&format!("{path}.value"))
        } else {
            original_value
        };
        let sealed_path = {
            let segments = split_escaped_path(path);
            let prefix = segments
                .iter()
                .take(segments.len().saturating_sub(1))
                .map(|segment| segment.raw.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if prefix.is_empty() {
                path.to_string()
            } else {
                format!("{prefix}.isSealed")
            }
        };
        let is_sealed =
            path != sealed_path && flattened_config.get(&sealed_path).is_some_and(is_truthy);
        let mut change = pretty_change(
            name_info,
            variable_original,
            Some(&patch_value),
            if patch_value.is_null() {
                ChangeType::Removed
            } else if variable_original.is_some() || is_sealed {
                ChangeType::Updated
            } else {
                ChangeType::Added
            },
            full_path,
            false,
            ResourceKind::Service,
            Some(service_id),
            Some(service_name),
        );
        change.is_sealed = is_sealed;
        return change;
    }

    if is_cpu_change {
        return PrettyChange {
            display_name: "CPU".into(),
            additional_info: Some("limit".into()),
            current_value: original_value
                .and_then(Value::as_f64)
                .map(|value| format!("{value} vCPU"))
                .unwrap_or_default(),
            new_value: patch_value
                .as_f64()
                .map(|value| format!("{value} vCPU"))
                .unwrap_or_default(),
            change_type: change_type(original_value, &patch_value),
            path: full_path.into(),
            is_destructive: false,
            is_sealed: false,
            resource_kind: ResourceKind::Service,
            resource_id: Some(service_id.into()),
            resource_name: Some(service_name.into()),
        };
    }

    if is_memory_change {
        return PrettyChange {
            display_name: "Memory".into(),
            additional_info: Some("limit".into()),
            current_value: original_value
                .and_then(Value::as_i64)
                .map(format_memory_bytes)
                .unwrap_or_default(),
            new_value: patch_value
                .as_i64()
                .map(format_memory_bytes)
                .unwrap_or_default(),
            change_type: change_type(original_value, &patch_value),
            path: full_path.into(),
            is_destructive: false,
            is_sealed: false,
            resource_kind: ResourceKind::Service,
            resource_id: Some(service_id.into()),
            resource_name: Some(service_name.into()),
        };
    }

    if is_volume_mounts_change {
        let segments = split_escaped_path(path);
        let volume_id = segments
            .get(1)
            .map(|segment| segment.key.clone())
            .unwrap_or_default();
        return pretty_change(
            DisplayName {
                additional_info: Some(volume_id),
                ..name_info
            },
            original_value,
            Some(&patch_value),
            change_type(original_value, &patch_value),
            full_path,
            false,
            ResourceKind::Service,
            Some(service_id),
            Some(service_name),
        );
    }

    if is_service_deleted {
        return PrettyChange {
            additional_info: Some("Service Removed".into()),
            current_value: String::new(),
            new_value: "REMOVED".into(),
            change_type: ChangeType::Removed,
            is_destructive: true,
            ..base_change(
                name_info,
                full_path,
                ResourceKind::Service,
                Some(service_id),
                Some(service_name),
            )
        };
    }

    if is_service_created {
        return PrettyChange {
            additional_info: Some("Service Created".into()),
            current_value: String::new(),
            new_value: "CREATED".into(),
            change_type: ChangeType::Added,
            ..base_change(
                name_info,
                full_path,
                ResourceKind::Service,
                Some(service_id),
                Some(service_name),
            )
        };
    }

    if is_tcp_proxy_change {
        let port = path.rsplit('.').next().unwrap_or_default();
        return PrettyChange {
            display_name: "Application Port".into(),
            additional_info: Some("TCP Proxy".into()),
            current_value: String::new(),
            new_value: port.into(),
            change_type: if patch_value.is_null() {
                ChangeType::Removed
            } else {
                ChangeType::Added
            },
            path: full_path.into(),
            is_destructive: false,
            is_sealed: false,
            resource_kind: ResourceKind::Service,
            resource_id: Some(service_id.into()),
            resource_name: Some(service_name.into()),
        };
    }

    if is_service_domain_change {
        let segments = split_escaped_path(path);
        let domain_key = segments
            .get(2)
            .map(|segment| segment.key.clone())
            .unwrap_or_default();
        return PrettyChange {
            display_name: "Service Domain".into(),
            additional_info: None,
            current_value: String::new(),
            new_value: service_domain_display_value(&domain_key),
            change_type: if patch_value.is_null() {
                ChangeType::Removed
            } else {
                ChangeType::Added
            },
            path: full_path.into(),
            is_destructive: false,
            is_sealed: false,
            resource_kind: ResourceKind::Service,
            resource_id: Some(service_id.into()),
            resource_name: Some(service_name.into()),
        };
    }

    if is_multi_region_config_change {
        return pretty_change(
            name_info,
            original_value,
            Some(&patch_value),
            if patch_value.is_null() {
                ChangeType::Removed
            } else {
                ChangeType::Added
            },
            full_path,
            false,
            ResourceKind::Service,
            Some(service_id),
            Some(service_name),
        );
    }

    if is_auto_updates_schedule {
        return PrettyChange {
            display_name: "Auto Update Schedule".into(),
            additional_info: None,
            current_value: display_value(original_value),
            new_value: display_value(Some(&patch_value)),
            change_type: change_type(original_value, &patch_value),
            path: format!("services.{service_id}.source.autoUpdates"),
            is_destructive: false,
            is_sealed: false,
            resource_kind: ResourceKind::Service,
            resource_id: Some(service_id.into()),
            resource_name: Some(service_name.into()),
        };
    }

    if full_path.ends_with(".startCommand") && is_function_service(service_patch, service_config) {
        return PrettyChange {
            display_name: "Source Code".into(),
            additional_info: None,
            current_value: original_value
                .and_then(Value::as_str)
                .map(function_source_code)
                .unwrap_or_default(),
            new_value: patch_value
                .as_str()
                .map(function_source_code)
                .unwrap_or_default(),
            change_type: change_type(original_value, &patch_value),
            path: full_path.into(),
            is_destructive: false,
            is_sealed: false,
            resource_kind: ResourceKind::Service,
            resource_id: Some(service_id.into()),
            resource_name: Some(service_name.into()),
        };
    }

    pretty_change(
        name_info,
        original_value,
        Some(&patch_value),
        change_type(original_value, &patch_value),
        full_path,
        false,
        ResourceKind::Service,
        Some(service_id),
        Some(service_name),
    )
}

fn collect_volume_changes(
    patch: &Value,
    environment_config: &Value,
    names: &ResourceNames,
    groups: &mut Vec<PrettyChangeGroup>,
) {
    collect_resource_changes(
        patch,
        environment_config,
        "volumes",
        ResourceKind::Volume,
        |id| names.volume_name(id),
        groups,
    );
}

fn collect_bucket_changes(
    patch: &Value,
    environment_config: &Value,
    names: &ResourceNames,
    groups: &mut Vec<PrettyChangeGroup>,
) {
    collect_resource_changes(
        patch,
        environment_config,
        "buckets",
        ResourceKind::Bucket,
        |id| names.bucket_name(id),
        groups,
    );
}

fn collect_group_changes(
    patch: &Value,
    environment_config: &Value,
    groups: &mut Vec<PrettyChangeGroup>,
) {
    collect_resource_changes(
        patch,
        environment_config,
        "groups",
        ResourceKind::Group,
        |id| id,
        groups,
    );
}

fn collect_resource_changes<'a>(
    patch: &'a Value,
    environment_config: &'a Value,
    key: &str,
    kind: ResourceKind,
    name: impl Fn(&'a str) -> &'a str,
    groups: &mut Vec<PrettyChangeGroup>,
) {
    let Some(resources) = patch.get(key).and_then(Value::as_object) else {
        return;
    };
    let mut entries = resources.iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    for (resource_id, resource_patch) in entries {
        let resource_config = environment_config
            .get(key)
            .and_then(|resources| resources.get(resource_id))
            .unwrap_or(&Value::Null);
        let flattened_patch = flatten_value(resource_patch);
        let flattened_config = flatten_value(resource_config);
        let mut changes = Vec::new();

        for (path, patch_value) in flattened_patch {
            let full_path = format!("{key}.{resource_id}.{path}");
            let name_info = get_change_display_name(&path);
            let original_value = flattened_config.get(&path);
            let is_deleted = path.ends_with("isDeleted") && patch_value == Value::Bool(true);
            let is_created = path.ends_with("isCreated") && patch_value == Value::Bool(true);
            let mut change = pretty_change(
                name_info,
                original_value,
                Some(&patch_value),
                change_type(original_value, &patch_value),
                &full_path,
                false,
                kind,
                Some(resource_id),
                Some(name(resource_id)),
            );

            if is_created {
                change.additional_info = Some(format!("{} Created", title_kind(kind)));
                change.current_value.clear();
                change.new_value = "CREATED".into();
                change.change_type = ChangeType::Added;
            } else if is_deleted {
                change.additional_info = Some(format!("{} Removed", title_kind(kind)));
                change.current_value.clear();
                change.new_value = "REMOVED".into();
                change.change_type = ChangeType::Removed;
                change.is_destructive = matches!(kind, ResourceKind::Volume | ResourceKind::Bucket);
            } else if path.starts_with("alerts.usage") {
                change.current_value = if patch_value.is_null() {
                    "SET".into()
                } else {
                    String::new()
                };
                change.new_value = if patch_value.is_null() {
                    "REMOVED".into()
                } else {
                    "SET".into()
                };
                change.is_destructive = patch_value.is_null();
            }

            changes.push(change);
        }

        if !changes.is_empty() {
            groups.push(change_group(
                kind,
                Some(resource_id.clone()),
                name(resource_id).to_string(),
                changes,
            ));
        }
    }
}

fn collect_shared_variable_changes(
    patch: &Value,
    environment_config: &Value,
    groups: &mut Vec<PrettyChangeGroup>,
) {
    let Some(shared_variables) = patch.get("sharedVariables") else {
        return;
    };
    let flattened_patch = flatten_value(shared_variables);
    let mut changes = Vec::new();

    for (path, value) in flattened_patch {
        let variable_name = split_escaped_path(&path)
            .first()
            .map(|segment| segment.key.clone())
            .unwrap_or_else(|| path.clone());
        let variable_name = variable_name.as_str();
        let original_value = environment_config
            .get("sharedVariables")
            .and_then(|vars| vars.get(variable_name))
            .and_then(|var| var.get("value"));
        changes.push(pretty_change(
            DisplayName {
                display_name: variable_name.into(),
                additional_info: Some("Shared Variable".into()),
            },
            original_value,
            Some(&value),
            change_type(original_value, &value),
            &format!("sharedVariables.{path}"),
            false,
            ResourceKind::SharedVariables,
            None,
            Some("Shared Variables"),
        ));
    }

    if !changes.is_empty() {
        groups.push(change_group(
            ResourceKind::SharedVariables,
            None,
            "Shared Variables".into(),
            changes,
        ));
    }
}

fn collect_private_networking_change(
    patch: &Value,
    environment_config: &Value,
    groups: &mut Vec<PrettyChangeGroup>,
) {
    let Some(value) = patch.get("privateNetworkDisabled") else {
        return;
    };
    let original = environment_config.get("privateNetworkDisabled");
    if original == Some(value) {
        return;
    }

    let change = PrettyChange {
        display_name: "Private Networking".into(),
        additional_info: None,
        current_value: if original.and_then(Value::as_bool).unwrap_or(false) {
            "Disabled".into()
        } else {
            "Enabled".into()
        },
        new_value: if value.as_bool().unwrap_or(false) {
            "Disabled".into()
        } else {
            "Enabled".into()
        },
        change_type: if value.as_bool().unwrap_or(false) {
            ChangeType::Removed
        } else {
            ChangeType::Added
        },
        path: "privateNetworkDisabled".into(),
        is_destructive: false,
        is_sealed: false,
        resource_kind: ResourceKind::Environment,
        resource_id: None,
        resource_name: Some("Environment".into()),
    };

    groups.push(change_group(
        ResourceKind::Environment,
        None,
        "Environment".into(),
        vec![change],
    ));
}

fn pretty_change(
    name_info: DisplayName,
    current_value: Option<&Value>,
    new_value: Option<&Value>,
    change_type: ChangeType,
    path: &str,
    is_destructive: bool,
    resource_kind: ResourceKind,
    resource_id: Option<&str>,
    resource_name: Option<&str>,
) -> PrettyChange {
    PrettyChange {
        display_name: name_info.display_name,
        additional_info: name_info.additional_info,
        current_value: display_value(current_value),
        new_value: display_value(new_value),
        change_type,
        path: path.into(),
        is_destructive,
        is_sealed: false,
        resource_kind,
        resource_id: resource_id.map(str::to_string),
        resource_name: resource_name.map(str::to_string),
    }
}

fn base_change(
    name_info: DisplayName,
    path: &str,
    resource_kind: ResourceKind,
    resource_id: Option<&str>,
    resource_name: Option<&str>,
) -> PrettyChange {
    PrettyChange {
        display_name: name_info.display_name,
        additional_info: name_info.additional_info,
        current_value: String::new(),
        new_value: String::new(),
        change_type: ChangeType::Updated,
        path: path.into(),
        is_destructive: false,
        is_sealed: false,
        resource_kind,
        resource_id: resource_id.map(str::to_string),
        resource_name: resource_name.map(str::to_string),
    }
}

fn change_group(
    resource_kind: ResourceKind,
    resource_id: Option<String>,
    resource_name: String,
    changes: Vec<PrettyChange>,
) -> PrettyChangeGroup {
    PrettyChangeGroup {
        resource_kind,
        resource_id,
        resource_name,
        summary: group_summary(&changes),
        changes,
    }
}

fn group_summary(changes: &[PrettyChange]) -> String {
    let variables = changes
        .iter()
        .filter(|change| {
            matches!(
                change.additional_info.as_deref(),
                Some("Variable" | "Variable generator" | "Shared Variable")
            )
        })
        .count();
    let settings = changes.len().saturating_sub(variables);
    let mut parts = Vec::new();
    if variables > 0 {
        parts.push(format!(
            "{} {}",
            variables,
            if variables == 1 {
                "Variable"
            } else {
                "Variables"
            }
        ));
    }
    if settings > 0 {
        parts.push(format!(
            "{} {}",
            settings,
            if settings == 1 { "Setting" } else { "Settings" }
        ));
    }
    parts.join(" - ")
}

#[derive(Debug, Clone)]
struct DisplayName {
    display_name: String,
    additional_info: Option<String>,
}

fn get_change_display_name(path: &str) -> DisplayName {
    let segments = split_escaped_path(path);
    let parts = segments
        .iter()
        .map(|segment| segment.key.as_str())
        .collect::<Vec<_>>();
    if parts.first() == Some(&"alerts") {
        return DisplayName {
            display_name: parts.last().unwrap_or(&path).to_string(),
            additional_info: Some("Volume Alert Usage".into()),
        };
    }
    if parts.first() == Some(&"variables") {
        let is_generator = parts.last() == Some(&"generator");
        return DisplayName {
            display_name: parts.get(1).unwrap_or(&"").to_string(),
            additional_info: Some(if is_generator {
                "Variable generator".into()
            } else {
                "Variable".into()
            }),
        };
    }
    if parts.len() > 3 && parts.get(1) == Some(&"multiRegionConfig") {
        match parts.get(3).copied() {
            Some("numReplicas") => {
                return DisplayName {
                    display_name: "Num Replicas".into(),
                    additional_info: parts.get(2).map(|part| (*part).into()),
                };
            }
            Some("stackersAssignment") => {
                return DisplayName {
                    display_name: "Stackers Assignment".into(),
                    additional_info: parts.get(2).map(|part| (*part).into()),
                };
            }
            _ => {}
        }
    }
    if parts.last() == Some(&"ipv6EgressEnabled") {
        return DisplayName {
            display_name: "Outbound IPv6".into(),
            additional_info: None,
        };
    }
    if parts.last() == Some(&"groupId") {
        return DisplayName {
            display_name: "Group".into(),
            additional_info: None,
        };
    }

    DisplayName {
        display_name: camel_case_to_title(parts.last().unwrap_or(&path)),
        additional_info: None,
    }
}

fn camel_case_to_title(text: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx == 0 {
            out.extend(ch.to_uppercase());
        } else {
            if ch.is_uppercase() {
                out.push(' ');
            }
            out.push(ch);
        }
    }
    out
}

fn display_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(value) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn format_memory_bytes(bytes: i64) -> String {
    if bytes <= 0 {
        return String::new();
    }
    const GB_THRESHOLD: f64 = 1000.0 * 1000.0 * 1000.0;
    if (bytes as f64) >= GB_THRESHOLD {
        let gb = bytes as f64 / GB_THRESHOLD;
        if (gb.fract()).abs() < f64::EPSILON {
            format!("{gb:.0} GB")
        } else {
            format!("{gb:.1} GB")
        }
    } else {
        format!("{} MB", (bytes as f64 / (1000.0 * 1000.0)).round() as i64)
    }
}

fn service_domain_display_value(domain_key: &str) -> String {
    if domain_key == "<hasDomain>" {
        return "Generated on Deploy".into();
    }
    if let Some(port) = domain_key.strip_prefix("<hasDomain>:") {
        return format!("Generated on Deploy (Port {port})");
    }
    domain_key.into()
}

fn function_source_code(start_command: &str) -> String {
    let encoded = start_command.split_whitespace().nth(1).unwrap_or_default();
    general_purpose::STANDARD
        .decode(encoded)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

fn is_function_service(service_patch: &Value, service_config: &Value) -> bool {
    service_patch
        .get("source")
        .or_else(|| service_config.get("source"))
        .and_then(|source| source.get("image"))
        .and_then(Value::as_str)
        .is_some_and(|image| image.to_lowercase().starts_with(FUNCTION_IMAGE_PREFIX))
}

fn is_truthy(value: &Value) -> bool {
    matches!(value, Value::Bool(true))
}

fn change_type(original_value: Option<&Value>, patch_value: &Value) -> ChangeType {
    if patch_value.is_null() {
        ChangeType::Removed
    } else if original_value.is_some() {
        ChangeType::Updated
    } else {
        ChangeType::Added
    }
}

fn service_sort_key(path: &str) -> usize {
    match path.split('.').next().unwrap_or_default() {
        "source" => 0,
        "networking" => 1,
        "build" => 2,
        "deploy" => 3,
        "configFile" => 4,
        "volumeMounts" => 5,
        "variables" => 6,
        _ => 99,
    }
}

fn title_kind(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Service => "Service",
        ResourceKind::Volume => "Volume",
        ResourceKind::Bucket => "Bucket",
        ResourceKind::Group => "Group",
        ResourceKind::SharedVariables => "Shared Variable",
        ResourceKind::Environment => "Environment",
    }
}

#[derive(Debug, Default)]
pub struct ResourceNames {
    services: BTreeMap<String, String>,
    volumes: BTreeMap<String, String>,
    buckets: BTreeMap<String, String>,
}

impl ResourceNames {
    fn from_context(
        project: &queries::RailwayProject,
        instances: &ProjectEnvironmentInstances,
    ) -> Self {
        let mut names = Self::default();
        for edge in &project.services.edges {
            names
                .services
                .insert(edge.node.id.clone(), edge.node.name.clone());
        }
        for edge in &project.buckets.edges {
            names
                .buckets
                .insert(edge.node.id.clone(), edge.node.name.clone());
        }
        for edge in &instances.volume_instances {
            names
                .volumes
                .insert(edge.node.volume.id.clone(), edge.node.volume.name.clone());
        }
        names
    }

    fn service_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.services.get(id).map(String::as_str).unwrap_or(id)
    }

    fn volume_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.volumes.get(id).map(String::as_str).unwrap_or(id)
    }

    fn bucket_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.buckets.get(id).map(String::as_str).unwrap_or(id)
    }
}

/// One segment of a dot path. `raw` keeps the escaped spelling used in
/// flattened paths; `key` is the literal object key (dots unescaped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSegment {
    pub raw: String,
    pub key: String,
}

/// Escapes literal dots in an object key for use inside a dot path, matching
/// the dashboard's `escapePath` (a variable named `FOO.BAR` flattens to
/// `variables.FOO\.BAR.value`).
pub fn escape_path_segment(key: &str) -> String {
    key.replace('.', "\\.")
}

/// Splits a dot path on unescaped dots only — the `(?<!\\)\.` rule. Unlike
/// the dashboard, we apply this in matching too, so dot-containing keys are
/// addressable.
pub fn split_escaped_path(path: &str) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    let mut raw = String::new();
    let mut key = String::new();
    let mut chars = path.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'.') => {
                chars.next();
                raw.push_str("\\.");
                key.push('.');
            }
            '.' => {
                segments.push(PathSegment {
                    raw: std::mem::take(&mut raw),
                    key: std::mem::take(&mut key),
                });
            }
            _ => {
                raw.push(ch);
                key.push(ch);
            }
        }
    }
    segments.push(PathSegment { raw, key });
    segments
}

pub fn flatten_value(value: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    flatten_value_inner(None, value, &mut out);
    out
}

fn flatten_value_inner(prefix: Option<String>, value: &Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(object) if !object.is_empty() => {
            for (key, value) in object {
                let segment = escape_path_segment(key);
                let path = match &prefix {
                    Some(prefix) => format!("{prefix}.{segment}"),
                    None => segment,
                };
                flatten_value_inner(Some(path), value, out);
            }
        }
        _ => {
            if let Some(prefix) = prefix {
                out.insert(prefix, value.clone());
            }
        }
    }
}

/// Matches a user pattern against a flattened path. Segments compare
/// escape-aware, `*` matches any single segment, and a shorter pattern
/// prefix-matches the whole subtree (`services.<id>` matches every staged
/// change under that service) — same prefix semantics as the dashboard's
/// `pathsMatch`.
pub fn paths_match(pattern: &str, path: &str) -> bool {
    let pattern_parts = split_escaped_path(pattern);
    let path_parts = split_escaped_path(path);
    pattern_parts.len() <= path_parts.len()
        && pattern_parts
            .iter()
            .zip(path_parts.iter())
            .all(|(pattern, part)| pattern.key == "*" || pattern.key == part.key)
}

fn remove_dot_path(value: &mut Value, path: &str) -> bool {
    let segments = split_escaped_path(path);
    let keys = segments
        .iter()
        .map(|segment| segment.key.as_str())
        .collect::<Vec<_>>();
    remove_path_parts(value, &keys)
}

fn remove_path_parts(value: &mut Value, parts: &[&str]) -> bool {
    let Value::Object(object) = value else {
        return false;
    };
    let Some((first, rest)) = parts.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return object.remove(*first).is_some();
    }

    let Some(child) = object.get_mut(*first) else {
        return false;
    };
    let removed = remove_path_parts(child, rest);
    if removed && matches!(child, Value::Object(map) if map.is_empty()) {
        object.remove(*first);
    }
    removed
}

/// Deep-merges the staged patch onto the live config, mirroring the
/// dashboard's `applyPatchToEnvironmentConfig`: objects merge recursively,
/// any other patch value (including null) wins.
pub fn apply_patch_to_config(config: &Value, patch: &Value) -> Value {
    match (config, patch) {
        (Value::Object(config), Value::Object(patch)) => {
            let mut merged = config.clone();
            for (key, patch_value) in patch {
                let entry = match merged.get(key) {
                    Some(existing) => apply_patch_to_config(existing, patch_value),
                    None => patch_value.clone(),
                };
                merged.insert(key.clone(), entry);
            }
            Value::Object(merged)
        }
        (_, patch) => patch.clone(),
    }
}

/// Port of the dashboard's `checkIfPatchRequiresTwoFactor`, evaluated on the
/// resolved (config + patch) environment: any service/volume/bucket deletion,
/// or a service whose region differs from a mounted pre-existing volume's
/// region (a cross-region volume migration).
pub fn patch_requires_two_factor(patch: &Value, current_config: &Value) -> bool {
    let resolved = apply_patch_to_config(current_config, patch);
    let empty = serde_json::Map::new();

    let services = resolved
        .get("services")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let volumes = resolved
        .get("volumes")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let buckets = resolved
        .get("buckets")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    for service in services.values() {
        if is_truthy_field(service, "isDeleted") {
            return true;
        }

        let service_region = service
            .get("deploy")
            .and_then(|deploy| deploy.get("multiRegionConfig"))
            .and_then(Value::as_object)
            .and_then(|regions| regions.keys().next().cloned())
            .or_else(|| {
                service
                    .get("deploy")
                    .and_then(|deploy| deploy.get("region"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });

        if let Some(service_region) = service_region {
            let mounted_volume = service
                .get("volumeMounts")
                .and_then(Value::as_object)
                .and_then(|mounts| mounts.iter().find(|(_, mount)| !mount.is_null()))
                .and_then(|(volume_id, _)| volumes.get(volume_id));

            if let Some(volume) = mounted_volume {
                let is_new = is_truthy_field(volume, "isCreated");
                let volume_region = volume.get("region").and_then(Value::as_str);
                if !is_new && volume_region != Some(service_region.as_str()) {
                    return true;
                }
            }
        }
    }

    volumes
        .values()
        .any(|volume| is_truthy_field(volume, "isDeleted"))
        || buckets
            .values()
            .any(|bucket| is_truthy_field(bucket, "isDeleted"))
}

fn is_truthy_field(value: &Value, field: &str) -> bool {
    value.get(field).is_some_and(is_truthy)
}

/// A resource the staged patch deletes, for the pre-commit destructive
/// warning (port of the dashboard's `getDeletedResources`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedResource {
    pub name: String,
    pub kind: String,
}

pub fn deleted_resources(view: &StagedChangesView) -> Vec<DeletedResource> {
    let patch = &view.patch.patch;
    let config = &view.current_config;
    let mut deleted = Vec::new();
    let empty = serde_json::Map::new();

    let patch_services = patch
        .get("services")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    for (service_id, service) in patch_services {
        if is_truthy_field(service, "isDeleted") {
            let instance = config
                .get("services")
                .and_then(|services| services.get(service_id));
            deleted.push(DeletedResource {
                name: resolve_name(view, ResourceKind::Service, service_id),
                kind: service_type(instance).into(),
            });
        }
    }

    for (key, kind) in [
        ("volumes", ResourceKind::Volume),
        ("buckets", ResourceKind::Bucket),
    ] {
        let resources = patch.get(key).and_then(Value::as_object).unwrap_or(&empty);
        for (resource_id, resource) in resources {
            if is_truthy_field(resource, "isDeleted") {
                deleted.push(DeletedResource {
                    name: resolve_name(view, kind, resource_id),
                    kind: kind.to_string(),
                });
            }
        }
    }

    let shared = patch
        .get("sharedVariables")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    for (name, value) in shared {
        if value.is_null() {
            deleted.push(DeletedResource {
                name: name.clone(),
                kind: "shared variable".into(),
            });
        }
    }

    deleted
}

/// Classifies a deleted service the way the dashboard's confirm modal does:
/// function (image prefix), cron (schedule set), database (image keyword),
/// else plain service.
fn service_type(instance: Option<&Value>) -> &'static str {
    let Some(instance) = instance else {
        return "service";
    };
    let image = instance
        .get("source")
        .and_then(|source| source.get("image"))
        .and_then(Value::as_str)
        .map(str::to_lowercase);

    if let Some(image) = &image {
        if image.starts_with(FUNCTION_IMAGE_PREFIX) {
            return "function";
        }
    }
    if instance
        .get("deploy")
        .and_then(|deploy| deploy.get("cronSchedule"))
        .is_some_and(|schedule| !schedule.is_null())
    {
        return "cron";
    }
    if let Some(image) = &image {
        if DATABASE_IMAGE_KEYWORDS
            .iter()
            .any(|keyword| image.contains(keyword))
        {
            return "database";
        }
    }
    "service"
}

fn resolve_name(view: &StagedChangesView, kind: ResourceKind, id: &str) -> String {
    // The pretty groups already carry names resolved from project metadata.
    view.pretty
        .groups
        .iter()
        .find(|group| group.resource_kind == kind && group.resource_id.as_deref() == Some(id))
        .map(|group| group.resource_name.clone())
        .unwrap_or_else(|| id.to_string())
}

/// The one-line notice every command that stages changes prints, so the whole
/// CLI points at the same next steps.
pub fn staged_changes_notice(count: usize) -> String {
    format!(
        "Staged {} {} — review: {} · deploy: {}",
        count,
        if count == 1 { "change" } else { "changes" },
        "railway changes status".cyan().bold(),
        "railway changes deploy".cyan().bold(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> ResourceNames {
        ResourceNames {
            services: BTreeMap::from([("svc_123".into(), "api".into())]),
            volumes: BTreeMap::new(),
            buckets: BTreeMap::new(),
        }
    }

    #[test]
    fn discards_nested_path_and_preserves_siblings() {
        let mut patch = json!({
            "services": {
                "svc_123": {
                    "deploy": {
                        "ipv6EgressEnabled": true,
                        "startCommand": "npm start"
                    }
                }
            }
        });

        assert!(remove_dot_path(
            &mut patch,
            "services.svc_123.deploy.ipv6EgressEnabled"
        ));
        assert_eq!(
            patch,
            json!({
                "services": {
                    "svc_123": {
                        "deploy": {
                            "startCommand": "npm start"
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn wildcard_paths_match_segments() {
        assert!(paths_match(
            "services.*.deploy.startCommand",
            "services.svc_123.deploy.startCommand"
        ));
        assert!(!paths_match(
            "services.*.build.startCommand",
            "services.svc_123.deploy.startCommand"
        ));
    }

    #[test]
    fn prefix_paths_match_whole_subtree() {
        assert!(paths_match(
            "services.svc_123",
            "services.svc_123.deploy.startCommand"
        ));
        assert!(paths_match(
            "services.*.deploy",
            "services.svc_123.deploy.startCommand"
        ));
        assert!(!paths_match(
            "services.svc_999",
            "services.svc_123.deploy.startCommand"
        ));
        // A longer pattern must not match a shorter path.
        assert!(!paths_match(
            "services.svc_123.deploy.startCommand",
            "services.svc_123.deploy"
        ));
    }

    #[test]
    fn escaped_dots_flatten_match_and_remove() {
        let mut patch = json!({
            "services": {
                "svc_123": {
                    "variables": {
                        "FOO.BAR": { "value": "1" },
                        "PLAIN": { "value": "2" }
                    }
                }
            }
        });

        let flattened = flatten_value(&patch);
        assert!(flattened.contains_key("services.svc_123.variables.FOO\\.BAR.value"));

        // The escaped pattern must match only the dotted-name variable...
        assert!(paths_match(
            "services.svc_123.variables.FOO\\.BAR",
            "services.svc_123.variables.FOO\\.BAR.value"
        ));
        // ...and must not be confused with a two-segment split.
        assert!(!paths_match(
            "services.svc_123.variables.FOO",
            "services.svc_123.variables.FOO\\.BAR.value"
        ));

        assert!(remove_dot_path(
            &mut patch,
            "services.svc_123.variables.FOO\\.BAR.value"
        ));
        assert_eq!(
            patch,
            json!({
                "services": {
                    "svc_123": {
                        "variables": {
                            "PLAIN": { "value": "2" }
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn two_factor_predicate_matches_dashboard() {
        let config = json!({
            "services": {
                "svc_123": {
                    "deploy": { "region": "us-west2" },
                    "volumeMounts": { "vol_1": { "mountPath": "/data" } }
                }
            },
            "volumes": { "vol_1": { "region": "us-west2" } }
        });

        // Service deletion requires 2FA.
        assert!(patch_requires_two_factor(
            &json!({ "services": { "svc_123": { "isDeleted": true } } }),
            &config
        ));
        // Bucket deletion requires 2FA.
        assert!(patch_requires_two_factor(
            &json!({ "buckets": { "bkt_1": { "isDeleted": true } } }),
            &config
        ));
        // Moving a service away from its mounted volume's region requires 2FA.
        assert!(patch_requires_two_factor(
            &json!({ "services": { "svc_123": { "deploy": { "region": "eu-west1" } } } }),
            &config
        ));
        // Same-region change does not.
        assert!(!patch_requires_two_factor(
            &json!({ "services": { "svc_123": { "deploy": { "startCommand": "npm start" } } } }),
            &config
        ));
        // Group deletion is deliberately not 2FA-gated.
        assert!(!patch_requires_two_factor(
            &json!({ "groups": { "grp_1": { "isDeleted": true } } }),
            &config
        ));
        // Newly created volumes don't gate region moves.
        assert!(!patch_requires_two_factor(
            &json!({
                "services": { "svc_new": { "deploy": { "region": "eu-west1" }, "volumeMounts": { "vol_new": {} } } },
                "volumes": { "vol_new": { "isCreated": true, "region": "us-west2" } }
            }),
            &json!({})
        ));
    }

    #[test]
    fn filter_matches_raw_path_represented_by_summarized_change() {
        let patch = json!({
            "services": {
                "svc_123": {
                    "source": {
                        "autoUpdates": {
                            "schedule": [{ "day": 1, "startHour": 2, "endHour": 3 }]
                        }
                    }
                }
            }
        });
        let pretty = prettify_patch(&patch, &json!({}), &names());
        let view = StagedChangesView {
            environment_id: "env_123".into(),
            environment_name: "production".into(),
            patch: StagedPatch {
                id: "patch_123".into(),
                status: "STAGED".into(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                last_applied_error: None,
                patch,
            },
            pretty,
            current_config: json!({}),
        };

        let filtered = filter_view_by_paths(
            &view,
            &["services.svc_123.source.autoUpdates.schedule".into()],
        )
        .unwrap();

        assert_eq!(filtered.pretty.total_changes, 1);
        assert_eq!(
            filtered.pretty.groups[0].changes[0].path,
            "services.svc_123.source.autoUpdates"
        );
    }

    #[test]
    fn raw_fallback_keeps_unknown_staged_paths_visible() {
        let patch = json!({
            "newTopLevelFeature": {
                "enabled": true
            }
        });

        let pretty = prettify_patch(&patch, &json!({}), &names());

        assert_eq!(pretty.total_changes, 1);
        assert_eq!(pretty.groups[0].resource_kind, ResourceKind::Environment);
        assert_eq!(
            pretty.groups[0].changes[0].path,
            "newTopLevelFeature.enabled"
        );
        assert_eq!(pretty.groups[0].changes[0].display_name, "Enabled");
        assert_eq!(
            pretty.groups[0].changes[0].additional_info.as_deref(),
            Some("Raw change")
        );
    }

    #[test]
    fn sealed_values_never_render_or_serialize() {
        let current = json!({
            "services": {
                "svc_123": {
                    "variables": {
                        "SECRET": { "value": "SECRET_VARIABLE_VALUE", "isSealed": true }
                    }
                }
            }
        });
        let patch = json!({
            "services": {
                "svc_123": {
                    "variables": {
                        "SECRET": { "value": "SECRET_VARIABLE_VALUE" }
                    }
                }
            }
        });

        let pretty = prettify_patch(&patch, &current, &names());
        let change = pretty
            .groups
            .iter()
            .flat_map(|group| group.changes.iter())
            .find(|change| change.display_name == "SECRET")
            .expect("sealed variable change");
        assert!(change.is_sealed);

        // Human rendering shows the sealed marker, never the sentinel.
        let rendered = render_value(change, &change.new_value, true);
        assert_eq!(rendered, "«sealed»");

        // JSON serialization nulls the values.
        let value = serde_json::to_value(change).unwrap();
        assert_eq!(value["currentValue"], Value::Null);
        assert_eq!(value["newValue"], Value::Null);
        assert_eq!(value["isSealed"], Value::Bool(true));
        assert!(!value.to_string().contains(SEALED_VALUE_SENTINEL));
    }
}
