use std::{
    env, fs,
    io::{Read, stdout},
    panic,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::{
    cursor::{Hide, Show},
    event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use is_terminal::IsTerminal;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph},
};
use tokio::{sync::mpsc, time::Instant};

use crate::{
    client::post_graphql,
    consts::TICK_STRING,
    controllers::{environment::get_matched_environment, project::get_project},
    errors::RailwayError,
    util::{
        progress::create_spinner_if,
        prompt::{
            fake_select, prompt_confirm_with_default, prompt_options,
            prompt_text_with_placeholder_disappear, prompt_text_with_placeholder_if_blank,
        },
        two_factor::validate_two_factor_if_enabled,
    },
    workspace::workspaces_with_client,
};

use super::*;

type TemplateSearchResponse = queries::template_search::ResponseData;
type TemplateSearchConnection = queries::template_search::TemplateSearchTemplateSearch;
type TemplateSearchEdge = queries::template_search::TemplateSearchTemplateSearchEdges;
type TemplateSearchItem = queries::template_search::TemplateSearchTemplateSearchEdgesNode;
type TemplateDetailItem = queries::template::TemplateTemplate;
type GeneratedTemplate = mutations::template_generate::TemplateGenerateTemplateGenerate;
type PublishedTemplate = mutations::template_publish::TemplatePublishTemplatePublish;

const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 50;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const RESULT_PADDING: &str = "  ";

const DESCRIPTION_MIN: usize = 25;
const DESCRIPTION_MAX: usize = 75;
const README_MIN: usize = 250;
const README_MAX: usize = 10_000;
const TEMPLATE_CATEGORIES: &[&str] = &[
    "AI/ML",
    "Analytics",
    "Authentication",
    "Automation",
    "Blogs",
    "Bots",
    "CMS",
    "Observability",
    "Other",
    "Starters",
    "Storage",
    "Queues",
];
const REQUIRED_README_SECTIONS: &[&str] = &[
    "# Deploy and Host",
    "## About Hosting",
    "## Why Deploy",
    "## Common Use Cases",
    "## Dependencies for",
    "### Deployment Dependencies",
];
const DEFAULT_README_TEXT: &[&str] = &[
    "[What is X?",
    "[Roughly 100 word",
    "[Use case",
    "[Dependency",
    "[Include any external links",
    "[Include Github",
];
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "avif", "gif", "svg", "ico"];
const README_SOURCE_EXISTING: &str = "Use existing README";
const README_SOURCE_GENERATED: &str = "Use generated README";
const README_SOURCE_FILE: &str = "Read from file";
const OPTIONAL_FIELD_CLEAR_HINT: &str = "none";

#[derive(Clone, Copy)]
enum TerminalTheme {
    Dark,
    Light,
}

/// Discover Railway templates
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway templates search postgres --json\n  railway templates create --project project-id --json\n  railway templates publish template-id --category Other --description \"Deploy and Host My App with Railway\" --readme-file README.md --json\n  railway templates unpublish template-code --yes --json\n  railway template find redis --limit 5 --json\n  railway templates ls --category database --json"
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Search published templates
    #[clap(visible_alias = "find", visible_alias = "list", visible_alias = "ls")]
    Search(SearchArgs),

    /// Create an unpublished template from a project
    #[clap(visible_alias = "generate")]
    Create(CreateArgs),

    /// Publish an unpublished template to the marketplace
    Publish(PublishArgs),

    /// Unpublish a published template from the marketplace
    Unpublish(UnpublishArgs),
}

#[derive(Parser, Clone)]
struct SearchArgs {
    /// Search term. Seeds the picker in TTY mode.
    query: Option<String>,

    /// Print the GraphQL response shape as JSON
    #[arg(long)]
    json: bool,

    /// Number of results to request
    #[arg(long, default_value_t = DEFAULT_LIMIT, value_parser = clap::value_parser!(i64).range(1..=MAX_LIMIT))]
    limit: i64,

    /// Fetch the next page using pageInfo.endCursor
    #[arg(long)]
    after: Option<String>,

    /// Filter by template category
    #[arg(long)]
    category: Option<String>,

    /// Filter by verification state
    #[arg(long)]
    verified: Option<bool>,
}

#[derive(Parser, Clone)]
#[clap(
    after_help = "Examples:\n\n  railway templates create --json\n  railway templates create --project project-id --environment production --json\n\nAutomation notes:\n  This matches the dashboard Generate Template action: it clones a project into an unpublished template draft.\n  The generated template opens in the dashboard template editor for cleanup before publishing.\n  In interactive mode, an omitted project is prompted."
)]
struct CreateArgs {
    /// Project ID or name. Defaults to the linked project.
    #[arg(short, long)]
    project: Option<String>,

    /// Environment ID or name. Defaults to the linked environment when available.
    #[arg(short, long)]
    environment: Option<String>,

    /// Print the created template as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Parser, Clone)]
#[clap(
    after_help = "Examples:\n\n  railway templates publish template-id --category Other --description \"Deploy and Host My App with Railway\" --readme-file README.md --json\n  railway templates publish template-id --category AI/ML --description \"Deploy and Host My Agent with Railway\" --readme-file - --json\n\nValid categories:\n  AI/ML, Analytics, Authentication, Automation, Blogs, Bots, CMS,\n  Observability, Other, Starters, Storage, Queues\n\nAutomation notes:\n  First publish requires a template overview via --readme-file or --readme.\n  In interactive mode, omitted template and metadata fields are prompted.\n  Use the template ID returned by `railway templates create --json`."
)]
struct PublishArgs {
    /// Template ID or code
    template: Option<String>,

    /// Marketplace category
    #[arg(long)]
    category: Option<String>,

    /// Short marketplace description
    #[arg(long)]
    description: Option<String>,

    /// Template overview markdown. Prefer --readme-file for multi-line content.
    #[arg(long)]
    readme: Option<String>,

    /// File containing the template overview markdown. Use "-" to read from stdin.
    #[arg(long)]
    readme_file: Option<PathBuf>,

    /// Image URL for the marketplace card
    #[arg(long)]
    image: Option<String>,

    /// Public demo project ID
    #[arg(long)]
    demo_project: Option<String>,

    /// Workspace ID or name. Defaults to the template workspace.
    #[arg(short, long)]
    workspace: Option<String>,

    /// Print the published template as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Parser, Clone)]
#[clap(
    after_help = "Examples:\n\n  railway templates unpublish template-code\n  railway templates unpublish template-id --yes --json\n\nAutomation notes:\n  Non-interactive unpublish requires --yes.\n  In interactive mode, omitted template is prompted."
)]
struct UnpublishArgs {
    /// Template ID or code
    template: Option<String>,

    /// Skip confirmation dialog
    #[arg(short = 'y', long = "yes")]
    yes: bool,

    /// 2FA code for verification when required by the current auth session
    #[arg(long = "2fa-code")]
    two_factor_code: Option<String>,

    /// Print the unpublished template result as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Clone)]
struct TemplateSearchRequest {
    query: String,
    limit: i64,
    after: Option<String>,
    category: Option<String>,
    verified: Option<bool>,
}

struct PickerApp {
    request: TemplateSearchRequest,
    results: Vec<TemplateSearchEdge>,
    selected: usize,
    theme: TerminalTheme,
    loading: bool,
    loading_more: bool,
    error: Option<String>,
    next_search_at: Option<Instant>,
    next_request_id: u64,
    active_request_id: u64,
    has_next_page: bool,
    end_cursor: Option<String>,
}

struct SearchMessage {
    request_id: u64,
    append: bool,
    result: Result<TemplateSearchConnection, String>,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Commands::Search(args) => search_command(args).await,
        Commands::Create(args) => create_command(args).await,
        Commands::Publish(args) => publish_command(args).await,
        Commands::Unpublish(args) => unpublish_command(args).await,
    }
}

async fn create_command(args: CreateArgs) -> Result<()> {
    let interactive = is_interactive_output(args.json);
    let configs = Configs::new()?;
    let client = GQLClient::new_user_authorized(&configs)?;
    let project = resolve_template_project(
        &client,
        &configs,
        args.project,
        args.environment,
        interactive,
    )
    .await?;

    let spinner = create_spinner_if(!args.json, "Creating template...".to_string());
    let response = post_graphql::<mutations::TemplateGenerate, _>(
        &client,
        configs.get_backboard(),
        mutations::template_generate::Variables {
            project_id: project.id,
            environment_id: project.environment_id,
        },
    )
    .await?;

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    let template = response.template_generate;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&template_json(&configs, &template))?
        );
    } else {
        print_created_template(&configs, &template);
    }

    Ok(())
}

async fn publish_command(args: PublishArgs) -> Result<()> {
    if args.readme.is_some() && args.readme_file.is_some() {
        bail!("Use either --readme or --readme-file, not both");
    }

    let interactive = is_interactive_output(args.json);
    let template_ref = resolve_template_ref(args.template.clone(), interactive, "publish")?;
    let configs = Configs::new()?;
    let client = GQLClient::new_user_authorized(&configs)?;
    let template = fetch_template_by_ref(&client, &configs, &template_ref).await?;
    let is_updating = matches!(
        &template.status,
        queries::template::TemplateStatus::PUBLISHED
    );

    let category = resolve_publish_category(args.category.clone(), &template, interactive)?;
    let description =
        resolve_publish_description(args.description.clone(), &template, interactive)?;
    let readme = resolve_readme(
        args.readme,
        args.readme_file,
        &template,
        is_updating,
        interactive,
    )?;
    let image = resolve_optional_publish_field(
        args.image.clone(),
        template.image.clone(),
        interactive,
        "Image URL",
        "<none>",
    )?;
    let demo_project_id = resolve_optional_publish_field(
        args.demo_project.clone(),
        template.demo_project_id.clone(),
        interactive,
        "Public demo project ID",
        "<none>",
    )?;
    let workspace_id = match args.workspace {
        Some(workspace) => Some(resolve_workspace_id(&client, &configs, &workspace).await?),
        None => template.workspace_id.clone(),
    };

    validate_publish_fields(
        &category,
        &description,
        readme.as_str(),
        image.as_deref(),
        is_updating,
    )?;

    if interactive
        && !confirm_publish(
            &template,
            &category,
            &description,
            &readme,
            image.as_deref(),
            demo_project_id.as_deref(),
            is_updating,
        )?
    {
        println!("Publish cancelled.");
        return Ok(());
    }

    let spinner = create_spinner_if(!args.json, "Publishing template...".to_string());
    let response = post_graphql::<mutations::TemplatePublish, _>(
        &client,
        configs.get_backboard(),
        mutations::template_publish::Variables {
            id: template.id,
            description,
            category,
            readme,
            image,
            demo_project_id,
            workspace_id,
        },
    )
    .await?;

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    let template = response.template_publish;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&published_template_json(&configs, &template))?
        );
    } else {
        print_published_template(&configs, &template, is_updating);
    }

    Ok(())
}

async fn unpublish_command(args: UnpublishArgs) -> Result<()> {
    let interactive = is_interactive_output(args.json);
    let template_ref = resolve_template_ref(args.template.clone(), interactive, "unpublish")?;
    let configs = Configs::new()?;
    let client = GQLClient::new_user_authorized(&configs)?;
    let template = fetch_template_by_ref(&client, &configs, &template_ref).await?;
    ensure_template_can_unpublish(&template.status, &template.name)?;

    if !args.yes {
        if !interactive {
            bail!(
                "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
            );
        }

        let confirmed = prompt_confirm_with_default(
            format!(
                r#"Unpublish template "{}" from the marketplace?"#,
                template.name.red()
            )
            .as_str(),
            false,
        )?;
        if !confirmed {
            println!("Unpublish cancelled.");
            return Ok(());
        }
    }

    validate_two_factor_if_enabled(&client, &configs, interactive, args.two_factor_code).await?;

    let spinner = create_spinner_if(!args.json, "Unpublishing template...".to_string());
    let response = post_graphql::<mutations::TemplateUnpublish, _>(
        &client,
        configs.get_backboard(),
        mutations::template_unpublish::Variables {
            id: template.id.clone(),
        },
    )
    .await?;

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": template.id,
                "code": template.code,
                "name": template.name,
                "unpublished": response.template_unpublish,
            }))?
        );
    } else {
        println!(
            "{} {} ({})",
            "Unpublished template".green().bold(),
            template.name.bold(),
            template.code
        );
    }

    Ok(())
}

fn ensure_template_can_unpublish(
    status: &queries::template::TemplateStatus,
    name: &str,
) -> Result<()> {
    if matches!(status, queries::template::TemplateStatus::PUBLISHED) {
        return Ok(());
    }

    bail!(
        "Template \"{}\" is {}. Only published templates can be unpublished.",
        name,
        status_label(status)
    )
}

#[derive(Clone)]
struct TemplateProjectChoice {
    id: String,
    name: String,
    workspace_name: String,
}

struct TemplateProjectContext {
    id: String,
    environment_id: Option<String>,
}

impl std::fmt::Display for TemplateProjectChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.workspace_name)
    }
}

async fn resolve_template_project(
    client: &reqwest::Client,
    configs: &Configs,
    project: Option<String>,
    environment: Option<String>,
    interactive: bool,
) -> Result<TemplateProjectContext> {
    if let Some(project) = project {
        let project_id = resolve_project_arg(client, configs, &project).await?;
        let environment_id = match environment {
            Some(environment) => {
                Some(resolve_project_environment(client, configs, &project_id, &environment).await?)
            }
            None => resolve_linked_environment_for_project(client, configs, &project_id).await?,
        };
        return Ok(TemplateProjectContext {
            id: project_id,
            environment_id,
        });
    }

    if let Ok(linked_project) = configs.get_linked_project().await {
        fake_select(
            "Select project",
            linked_project
                .name
                .as_deref()
                .unwrap_or(linked_project.project.as_str()),
        );
        let environment_id = match environment.or(linked_project.environment) {
            Some(environment) => Some(
                resolve_project_environment(client, configs, &linked_project.project, &environment)
                    .await?,
            ),
            None => None,
        };
        return Ok(TemplateProjectContext {
            id: linked_project.project,
            environment_id,
        });
    }

    if !interactive {
        bail!("Project required in non-interactive mode. Use --project <id or name>.");
    }

    let choices = project_choices(client, configs).await?;
    if choices.is_empty() {
        bail!(RailwayError::NoProjects);
    }

    let choice = prompt_options("Select project", choices)?;
    let environment_id = match environment {
        Some(environment) => {
            Some(resolve_project_environment(client, configs, &choice.id, &environment).await?)
        }
        None => None,
    };
    Ok(TemplateProjectContext {
        id: choice.id,
        environment_id,
    })
}

async fn resolve_project_arg(
    client: &reqwest::Client,
    configs: &Configs,
    project: &str,
) -> Result<String> {
    match get_project(client, configs, project.to_string()).await {
        Ok(project) => {
            fake_select("Select project", &project.name);
            return Ok(project.id);
        }
        Err(RailwayError::ProjectNotFound) => {}
        Err(RailwayError::GraphQLError(message)) if message.contains("Project not found") => {}
        Err(error) => return Err(error.into()),
    }

    let choice = find_project_choice(client, configs, project).await?;
    fake_select("Select project", &choice.to_string());
    Ok(choice.id)
}

async fn resolve_linked_environment_for_project(
    client: &reqwest::Client,
    configs: &Configs,
    project_id: &str,
) -> Result<Option<String>> {
    let Ok(linked_project) = configs.get_linked_project().await else {
        return Ok(None);
    };

    if linked_project.project != project_id {
        return Ok(None);
    }

    match linked_project.environment {
        Some(environment) => Ok(Some(
            resolve_project_environment(client, configs, project_id, &environment).await?,
        )),
        None => Ok(None),
    }
}

async fn resolve_project_environment(
    client: &reqwest::Client,
    configs: &Configs,
    project_id: &str,
    environment: &str,
) -> Result<String> {
    let project = get_project(client, configs, project_id.to_string()).await?;
    if project.deleted_at.is_some() {
        bail!(RailwayError::ProjectDeleted);
    }

    let environment = get_matched_environment(&project, environment.to_string())?;
    if environment.deleted_at.is_some() {
        bail!(RailwayError::EnvironmentDeleted);
    }

    fake_select("Select environment", &environment.name);
    Ok(environment.id)
}

async fn find_project_choice(
    client: &reqwest::Client,
    configs: &Configs,
    project: &str,
) -> Result<TemplateProjectChoice> {
    let choices = project_choices(client, configs).await?;
    let id_matches = choices
        .iter()
        .filter(|choice| choice.id.eq_ignore_ascii_case(project))
        .cloned()
        .collect::<Vec<_>>();

    if let Some(choice) = single_match(id_matches, project, "project ID")? {
        return Ok(choice);
    }

    let name_matches = choices
        .into_iter()
        .filter(|choice| choice.name.eq_ignore_ascii_case(project))
        .collect::<Vec<_>>();

    if let Some(choice) = single_match(name_matches, project, "project name")? {
        return Ok(choice);
    }

    bail!("Project \"{}\" not found", project)
}

fn single_match(
    matches: Vec<TemplateProjectChoice>,
    input: &str,
    kind: &str,
) -> Result<Option<TemplateProjectChoice>> {
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => {
            let available = matches
                .iter()
                .map(|choice| format!("{} ({})", choice.id, choice.workspace_name))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("Ambiguous {kind} \"{input}\". Use one of these project IDs: {available}");
        }
    }
}

async fn project_choices(
    client: &reqwest::Client,
    configs: &Configs,
) -> Result<Vec<TemplateProjectChoice>> {
    let mut choices = Vec::new();
    for workspace in workspaces_with_client(client, configs).await? {
        let workspace_name = workspace.name().to_string();
        choices.extend(
            workspace
                .projects()
                .into_iter()
                .filter(|project| project.deleted_at().is_none())
                .map(|project| TemplateProjectChoice {
                    id: project.id().to_string(),
                    name: project.name().to_string(),
                    workspace_name: workspace_name.clone(),
                }),
        );
    }
    Ok(choices)
}

async fn fetch_template_by_ref(
    client: &reqwest::Client,
    configs: &Configs,
    template_ref: &str,
) -> Result<TemplateDetailItem> {
    match fetch_template(client, configs, Some(template_ref.to_string()), None).await {
        Ok(template) => Ok(template),
        Err(RailwayError::GraphQLError(message)) if message.contains("Template not found") => {
            fetch_template(client, configs, None, Some(template_ref.to_string()))
                .await
                .map_err(Into::into)
        }
        Err(error) => Err(error.into()),
    }
}

async fn fetch_template(
    client: &reqwest::Client,
    configs: &Configs,
    id: Option<String>,
    code: Option<String>,
) -> Result<TemplateDetailItem, RailwayError> {
    let response = post_graphql::<queries::Template, _>(
        client,
        configs.get_backboard(),
        queries::template::Variables { id, code },
    )
    .await?;
    Ok(response.template)
}

async fn resolve_workspace_id(
    client: &reqwest::Client,
    configs: &Configs,
    workspace: &str,
) -> Result<String> {
    let matches = workspaces_with_client(client, configs)
        .await?
        .into_iter()
        .filter(|candidate| {
            candidate.id().eq_ignore_ascii_case(workspace)
                || candidate.name().eq_ignore_ascii_case(workspace)
                || candidate
                    .team_id()
                    .is_some_and(|team_id| team_id.eq_ignore_ascii_case(workspace))
        })
        .collect::<Vec<_>>();

    match matches.len() {
        0 => bail!(RailwayError::WorkspaceNotFound(workspace.to_string())),
        1 => {
            let workspace = matches[0].clone();
            fake_select("Select workspace", workspace.name());
            Ok(workspace.id().to_string())
        }
        _ => bail!(
            "Workspace \"{}\" is ambiguous. Use the workspace ID.",
            workspace
        ),
    }
}

fn resolve_template_ref(
    template: Option<String>,
    interactive: bool,
    action: &str,
) -> Result<String> {
    if let Some(template) = template.and_then(non_empty_string) {
        return Ok(template);
    }

    if !interactive {
        bail!("Template required in non-interactive mode. Pass a template ID or code to {action}.");
    }

    let template =
        prompt_text_with_placeholder_disappear("Template ID or code", "template-id-or-code")?;
    non_empty_string(template).with_context(|| "Template ID or code is required")
}

fn resolve_publish_category(
    category: Option<String>,
    template: &TemplateDetailItem,
    interactive: bool,
) -> Result<String> {
    if let Some(category) = category.and_then(non_empty_string) {
        return Ok(category);
    }

    let default = template
        .category
        .clone()
        .and_then(non_empty_string)
        .filter(|category| is_template_category(category))
        .unwrap_or_else(|| "Other".to_string());

    if interactive {
        return prompt_options("Select category", category_options(&default));
    }

    fake_select("Select category", &default);
    Ok(default)
}

fn resolve_publish_description(
    description: Option<String>,
    template: &TemplateDetailItem,
    interactive: bool,
) -> Result<String> {
    if let Some(description) = description.and_then(non_empty_string) {
        return Ok(description);
    }

    let default = template
        .description
        .clone()
        .and_then(non_empty_string)
        .unwrap_or_else(|| short_description(&template.name));

    if interactive {
        let description =
            prompt_text_with_placeholder_if_blank("Short description", &default, &default)?;
        return Ok(non_empty_string(description).unwrap_or(default));
    }

    Ok(default)
}

fn resolve_readme(
    readme: Option<String>,
    readme_file: Option<PathBuf>,
    template: &TemplateDetailItem,
    is_updating: bool,
    interactive: bool,
) -> Result<String> {
    if let Some(readme) = readme {
        fake_select("Template overview", "<provided inline>");
        return Ok(readme);
    }

    if let Some(path) = readme_file {
        return read_readme_file(path);
    }

    let existing_readme = template.readme.as_ref().and_then(|readme| {
        if readme.trim().is_empty() {
            None
        } else {
            Some(readme.clone())
        }
    });

    if interactive {
        if let Some(existing_readme) = existing_readme {
            let choice = prompt_options(
                "Template overview",
                vec![
                    README_SOURCE_EXISTING.to_string(),
                    README_SOURCE_FILE.to_string(),
                ],
            )?;
            if choice == README_SOURCE_EXISTING {
                return Ok(existing_readme);
            }
            return prompt_readme_file();
        }

        if is_updating {
            let choice = prompt_options(
                "Template overview",
                vec![
                    README_SOURCE_GENERATED.to_string(),
                    README_SOURCE_FILE.to_string(),
                ],
            )?;
            if choice == README_SOURCE_GENERATED {
                return Ok(default_readme(&template.name));
            }
            return prompt_readme_file();
        }

        return prompt_readme_file();
    }

    if let Some(readme) = existing_readme {
        fake_select("Template overview", "<existing template readme>");
        return Ok(readme);
    }

    if is_updating {
        return Ok(default_readme(&template.name));
    }

    if !interactive {
        bail!("Template overview required. Use --readme-file <path> or --readme <markdown>.");
    }

    prompt_readme_file()
}

fn prompt_readme_file() -> Result<String> {
    let path =
        prompt_text_with_placeholder_if_blank("Template overview file", "README.md", "README.md")?;
    let path = non_empty_string(path).unwrap_or_else(|| "README.md".to_string());
    read_readme_file(PathBuf::from(path))
}

fn read_readme_file(path: PathBuf) -> Result<String> {
    if path.as_os_str() == "-" {
        fake_select("Template overview file", "<stdin>");
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        return Ok(input);
    }

    fake_select("Template overview file", &path.display().to_string());
    Ok(fs::read_to_string(&path)
        .with_context(|| format!("Failed to read template overview from {}", path.display()))?)
}

fn resolve_optional_publish_field(
    value: Option<String>,
    existing: Option<String>,
    interactive: bool,
    message: &str,
    empty_placeholder: &str,
) -> Result<Option<String>> {
    if let Some(value) = value {
        return Ok(normalize_optional_publish_value(value));
    }

    let existing = existing.and_then(non_empty_string);
    if !interactive {
        return Ok(existing);
    }

    let default = existing.unwrap_or_default();
    let placeholder = if default.is_empty() {
        empty_placeholder
    } else {
        &default
    };
    let prompt = if default.is_empty() {
        message.to_string()
    } else {
        format!("{message} (type {OPTIONAL_FIELD_CLEAR_HINT} to clear)")
    };
    let value = prompt_text_with_placeholder_if_blank(&prompt, placeholder, &default)?;
    if is_optional_field_clear_value(&value) {
        return Ok(None);
    }

    Ok(non_empty_string(value).or_else(|| non_empty_string(default)))
}

fn normalize_optional_publish_value(value: String) -> Option<String> {
    if is_optional_field_clear_value(&value) {
        None
    } else {
        non_empty_string(value)
    }
}

fn is_optional_field_clear_value(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "none" | "clear")
}

fn validate_publish_fields(
    category: &str,
    description: &str,
    readme: &str,
    image: Option<&str>,
    is_updating: bool,
) -> Result<()> {
    validate_description(description)?;
    validate_category(category)?;
    if !is_updating {
        validate_readme(readme)?;
    }
    if let Some(image) = image {
        validate_image(image)?;
    }
    Ok(())
}

fn confirm_publish(
    template: &TemplateDetailItem,
    category: &str,
    description: &str,
    readme: &str,
    image: Option<&str>,
    demo_project_id: Option<&str>,
    is_updating: bool,
) -> Result<bool> {
    println!();
    println!("Review template:");
    println!("  Template: {}", template.name);
    println!("  Category: {category}");
    println!("  Description: {description}");
    println!("  README: {} characters", js_string_len(readme));
    println!("  Image: {}", image.unwrap_or("none"));
    println!("  Demo project: {}", demo_project_id.unwrap_or("none"));
    println!();

    let message = if is_updating {
        "Update template info?"
    } else {
        "Publish template to the marketplace?"
    };
    prompt_confirm_with_default(message, true)
}

fn validate_description(description: &str) -> Result<()> {
    let len = js_string_len(description.trim());
    if len < DESCRIPTION_MIN {
        bail!("description: Must be {DESCRIPTION_MIN} or more characters long");
    }
    if len > DESCRIPTION_MAX {
        bail!("description: Must be {DESCRIPTION_MAX} or fewer characters long");
    }
    Ok(())
}

fn validate_category(category: &str) -> Result<()> {
    if !is_template_category(category) {
        bail!(
            "category: Invalid category. Valid categories: {}",
            TEMPLATE_CATEGORIES.join(", ")
        );
    }
    Ok(())
}

fn is_template_category(category: &str) -> bool {
    TEMPLATE_CATEGORIES
        .iter()
        .any(|item| *item == category.trim())
}

fn category_options(default: &str) -> Vec<String> {
    let mut options = vec![default.to_string()];
    options.extend(
        TEMPLATE_CATEGORIES
            .iter()
            .filter(|category| **category != default)
            .map(|category| category.to_string()),
    );
    options
}

fn validate_readme(readme: &str) -> Result<()> {
    let len = js_string_len(readme.trim());
    if len < README_MIN {
        bail!("readme: Must be {README_MIN} or more characters long");
    }
    if len > README_MAX {
        bail!("readme: Must be {README_MAX} or fewer characters long");
    }

    let missing = REQUIRED_README_SECTIONS
        .iter()
        .filter(|section| !readme.contains(**section))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!("readme: Missing required sections: {}", missing.join(", "));
    }

    if DEFAULT_README_TEXT
        .iter()
        .any(|default_text| readme.contains(default_text))
    {
        bail!("readme: Please update the default text in each section.");
    }

    Ok(())
}

fn validate_image(image: &str) -> Result<()> {
    let image = image.trim();
    if image.is_empty() {
        return Ok(());
    }

    let path = if let Some(path) = image.strip_prefix("http://") {
        path
    } else if let Some(path) = image.strip_prefix("https://") {
        path
    } else {
        bail!("image: Invalid image URL");
    };

    if image.contains(['\n', '\r'])
        || !IMAGE_EXTENSIONS.iter().any(|extension| {
            let suffix = format!(".{extension}");
            image.ends_with(&suffix) && path.len() > suffix.len()
        })
    {
        bail!("image: Invalid image URL");
    }

    Ok(())
}

fn js_string_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn is_interactive_output(json: bool) -> bool {
    std::io::stdout().is_terminal() && !json
}

fn short_description(name: &str) -> String {
    format!("Deploy and Host {name} with Railway")
}

fn default_readme(name: &str) -> String {
    format!(
        "# Deploy and Host {name} on Railway\n\n\
         ## About Hosting {name}\n\n\
         ## Common Use Cases\n\n\
         ## Dependencies for {name} Hosting\n\n\
         ### Deployment Dependencies\n\n\
         ## Why Deploy {name} on Railway?\n\n\
         Railway is a singular platform to deploy your infrastructure stack. Railway will host your infrastructure so you don't have to deal with configuration, while allowing you to vertically and horizontally scale it.\n"
    )
}

fn non_empty_string(value: String) -> Option<String> {
    let value = value.trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn print_created_template(configs: &Configs, template: &GeneratedTemplate) {
    println!(
        "{} {} ({})",
        "Created template".green().bold(),
        template.name.bold(),
        template.id
    );
    println!("Status: {}", status_label(&template.status));
    println!();
    println!("Edit in dashboard:");
    println!(
        "  {}",
        template_editor_url(configs, &template.id)
            .bold()
            .underline()
    );
    println!();
    println!("Publish with:");
    println!(
        "  railway templates publish {} --category Other --description {} --readme-file README.md",
        shell_arg(&template.id),
        shell_arg(&short_description(&template.name))
    );
}

fn print_published_template(configs: &Configs, template: &PublishedTemplate, is_updating: bool) {
    let action = if is_updating {
        "Updated template"
    } else {
        "Published template"
    };
    println!(
        "{} {} ({})",
        action.green().bold(),
        template.name.bold(),
        template.code
    );
    println!("Status: {}", status_label(&template.status));
    println!();
    println!(
        "{}",
        template_url(configs, &template.code).bold().underline()
    );
}

fn template_json(configs: &Configs, template: &GeneratedTemplate) -> serde_json::Value {
    serde_json::json!({
        "id": template.id,
        "code": template.code,
        "name": template.name,
        "description": template.description,
        "status": status_label(&template.status),
        "workspaceId": template.workspace_id,
        "editorUrl": template_editor_url(configs, &template.id),
    })
}

fn published_template_json(configs: &Configs, template: &PublishedTemplate) -> serde_json::Value {
    serde_json::json!({
        "id": template.id,
        "code": template.code,
        "name": template.name,
        "description": template.description,
        "image": template.image,
        "category": template.category,
        "readme": template.readme,
        "demoProjectId": template.demo_project_id,
        "status": status_label(&template.status),
        "workspaceId": template.workspace_id,
        "url": template_url(configs, &template.code),
    })
}

fn template_editor_url(configs: &Configs, template_id: &str) -> String {
    format!(
        "https://{}/workspace/templates/{template_id}",
        configs.get_host()
    )
}

fn template_url(configs: &Configs, code: &str) -> String {
    template_url_from_host(configs.get_host(), code)
}

fn template_url_from_host(host: &str, code: &str) -> String {
    format!("https://{host}/deploy/{code}")
}

fn status_label<T: std::fmt::Debug>(status: &T) -> String {
    format!("{status:?}")
}

async fn search_command(args: SearchArgs) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_public()?;
    let backboard = configs.get_backboard();
    let request = TemplateSearchRequest {
        query: args.query.unwrap_or_default(),
        limit: args.limit,
        after: args.after,
        category: args.category,
        verified: args.verified,
    };

    if std::io::stdout().is_terminal() && !args.json {
        if let Some(template) = run_picker(client, backboard, request).await? {
            print_selected_template(&template);
        }
        return Ok(());
    }

    let response = fetch_template_search(&client, &backboard, &request).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        print_template_results(&request, &response.template_search);
    }

    Ok(())
}

async fn fetch_template_search(
    client: &reqwest::Client,
    backboard: &str,
    request: &TemplateSearchRequest,
) -> Result<TemplateSearchResponse> {
    let vars = queries::template_search::Variables {
        query: request.query.clone(),
        first: Some(request.limit),
        after: request.after.clone(),
        verified: request.verified,
        category: request.category.clone(),
    };

    Ok(post_graphql::<queries::TemplateSearch, _>(client, backboard, vars).await?)
}

fn spawn_search(
    tx: mpsc::UnboundedSender<SearchMessage>,
    client: reqwest::Client,
    backboard: String,
    request: TemplateSearchRequest,
    request_id: u64,
    append: bool,
) {
    tokio::spawn(async move {
        let result = fetch_template_search(&client, &backboard, &request)
            .await
            .map(|response| response.template_search)
            .map_err(|e| format!("{e:#}"));
        let _ = tx.send(SearchMessage {
            request_id,
            append,
            result,
        });
    });
}

async fn run_picker(
    client: reqwest::Client,
    backboard: String,
    request: TemplateSearchRequest,
) -> Result<Option<TemplateSearchItem>> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let (mut terminal, theme) = setup_terminal()?;
    let _cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    let (search_tx, mut search_rx) = mpsc::unbounded_channel();
    let mut app = PickerApp {
        request,
        results: Vec::new(),
        selected: 0,
        theme,
        loading: true,
        loading_more: false,
        error: None,
        next_search_at: None,
        next_request_id: 1,
        active_request_id: 1,
        has_next_page: false,
        end_cursor: None,
    };

    spawn_search(
        search_tx.clone(),
        client.clone(),
        backboard.clone(),
        app.request.clone(),
        app.active_request_id,
        false,
    );

    let mut events = EventStream::new();
    let mut render_interval = tokio::time::interval(FRAME_INTERVAL);
    render_interval.tick().await;

    loop {
        tokio::select! {
            _ = render_interval.tick() => {
                terminal.draw(|frame| render_picker(&app, frame))?;
            }
            Some(message) = search_rx.recv() => {
                if message.request_id == app.active_request_id {
                    app.loading = false;
                    app.loading_more = false;
                    match message.result {
                        Ok(connection) => {
                            app.error = None;
                            app.has_next_page = connection.page_info.has_next_page;
                            app.end_cursor = connection.page_info.end_cursor;
                            if message.append {
                                app.results.extend(connection.edges);
                            } else {
                                app.results = connection.edges;
                                app.selected = 0;
                            }
                            app.selected = app.selected.min(app.results.len().saturating_sub(1));
                        }
                        Err(error) => {
                            if !message.append {
                                app.results.clear();
                                app.selected = 0;
                                app.has_next_page = false;
                                app.end_cursor = None;
                            }
                            app.error = Some(error);
                        }
                    }
                }
            }
            Some(Ok(event)) = events.next() => {
                if let Some(template) = handle_picker_event(event, &mut app) {
                    return Ok(template);
                }
                maybe_load_more(
                    &mut app,
                    &search_tx,
                    &client,
                    &backboard,
                );
            }
            _ = wait_for_debounce(app.next_search_at), if app.next_search_at.is_some() => {
                app.next_search_at = None;
                app.next_request_id += 1;
                app.active_request_id = app.next_request_id;
                app.loading = true;
                app.error = None;
                spawn_search(
                    search_tx.clone(),
                    client.clone(),
                    backboard.clone(),
                    app.request.clone(),
                    app.active_request_id,
                    false,
                );
            }
            _ = tokio::signal::ctrl_c() => {
                return Ok(None);
            }
        }
    }
}

async fn wait_for_debounce(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    }
}

fn handle_picker_event(event: Event, app: &mut PickerApp) -> Option<Option<TemplateSearchItem>> {
    let Event::Key(key) = event else {
        return None;
    };
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }

    match key.code {
        KeyCode::Esc => Some(None),
        KeyCode::Enter => app
            .results
            .get(app.selected)
            .map(|edge| Some(edge.node.clone())),
        KeyCode::Up => {
            app.selected = app.selected.saturating_sub(1);
            None
        }
        KeyCode::Down => {
            if !app.results.is_empty() {
                app.selected = (app.selected + 1).min(app.results.len() - 1);
            }
            None
        }
        KeyCode::PageUp => {
            app.selected = app.selected.saturating_sub(5);
            None
        }
        KeyCode::PageDown => {
            if !app.results.is_empty() {
                app.selected = (app.selected + 5).min(app.results.len() - 1);
            }
            None
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(None),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request.query.clear();
            queue_picker_search(app);
            None
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request.query.push(ch);
            queue_picker_search(app);
            None
        }
        KeyCode::Backspace => {
            app.request.query.pop();
            queue_picker_search(app);
            None
        }
        KeyCode::Delete => {
            app.request.query.clear();
            queue_picker_search(app);
            None
        }
        _ => None,
    }
}

fn queue_picker_search(app: &mut PickerApp) {
    app.selected = 0;
    app.request.after = None;
    app.error = None;
    app.loading = true;
    app.loading_more = false;
    app.next_search_at = Some(Instant::now() + SEARCH_DEBOUNCE);
}

fn maybe_load_more(
    app: &mut PickerApp,
    search_tx: &mpsc::UnboundedSender<SearchMessage>,
    client: &reqwest::Client,
    backboard: &str,
) {
    if app.loading
        || app.loading_more
        || app.next_search_at.is_some()
        || !app.has_next_page
        || app.results.is_empty()
    {
        return;
    }

    if app.results.len().saturating_sub(app.selected) > 4 {
        return;
    }

    let Some(cursor) = app.end_cursor.clone() else {
        return;
    };

    let mut request = app.request.clone();
    request.after = Some(cursor);
    app.next_request_id += 1;
    app.active_request_id = app.next_request_id;
    app.loading_more = true;
    spawn_search(
        search_tx.clone(),
        client.clone(),
        backboard.to_string(),
        request,
        app.active_request_id,
        true,
    );
}

fn setup_terminal() -> Result<(Terminal<CrosstermBackend<std::io::Stdout>>, TerminalTheme)> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let theme = detect_terminal_theme();
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok((terminal, theme))
}

fn restore_terminal() {
    let _ = execute!(stdout(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

fn render_picker(app: &PickerApp, frame: &mut Frame) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    if area.width < 48 || area.height < 12 {
        let warning = Paragraph::new("Terminal too small. Resize to search templates.")
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(warning, area);
        return;
    }

    let width = area.width.saturating_sub(8).min(96);
    let height = area.height.saturating_sub(2);
    let content = Rect {
        x: area.x + 4,
        y: area.y + 1,
        width,
        height,
    };
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(content);

    render_search_input(app, frame, chunks[0]);
    render_picker_list(app, frame, chunks[2]);
    render_picker_hint(app, frame, chunks[3]);
}

fn render_search_input(app: &PickerApp, frame: &mut Frame, area: Rect) {
    let input = if app.request.query.is_empty() {
        Line::from(Span::styled(
            "Search templates...",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Line::from(Span::raw(app.request.query.clone()))
    };

    let input = Paragraph::new(input).block(
        Block::default()
            .borders(Borders::ALL)
            .padding(Padding::new(1, 1, 0, 0))
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(input, area);
}

fn render_picker_list(app: &PickerApp, frame: &mut Frame, area: Rect) {
    if let Some(error) = &app.error {
        let message = Paragraph::new(format!("Search failed: {error}"))
            .style(Style::default().fg(Color::Red));
        frame.render_widget(message, area);
        return;
    }

    if app.results.is_empty() {
        let paragraph = if app.loading {
            Paragraph::new(Line::from(vec![
                Span::styled("Searching templates ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    spinner_frame().to_string(),
                    Style::default().fg(Color::Green),
                ),
            ]))
        } else {
            Paragraph::new("No templates found.").style(Style::default().fg(Color::DarkGray))
        };
        frame.render_widget(paragraph, area);
        return;
    }

    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(idx, edge)| template_list_item(&edge.node, idx == app.selected, app.theme))
        .collect();
    let list = List::new(items);
    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_picker_hint(app: &PickerApp, frame: &mut Frame, area: Rect) {
    let result_count = if app.results.is_empty() {
        String::new()
    } else {
        format!("  {} results", app.results.len())
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            "Enter select  Up/Down move  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(result_count, Style::default().fg(Color::DarkGray)),
    ]));
    frame.render_widget(footer, area);
}

fn spinner_frame() -> char {
    let frame = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| (duration.as_millis() / 100) as usize)
        .unwrap_or_default();
    let frame_count = TICK_STRING.chars().count();

    if frame_count == 0 {
        return ' ';
    }

    TICK_STRING.chars().nth(frame % frame_count).unwrap_or(' ')
}

fn detect_terminal_theme() -> TerminalTheme {
    terminal_theme_from_colorfgbg()
        .or_else(query_terminal_background)
        .unwrap_or(TerminalTheme::Light)
}

fn terminal_theme_from_colorfgbg() -> Option<TerminalTheme> {
    let value = env::var("COLORFGBG").ok()?;
    let background = value.split(';').next_back()?.parse::<u8>().ok()?;

    if matches!(background, 7 | 9..=15) {
        Some(TerminalTheme::Light)
    } else {
        Some(TerminalTheme::Dark)
    }
}

#[cfg(unix)]
fn query_terminal_background() -> Option<TerminalTheme> {
    use nix::libc;
    use std::{
        io::{Read, Write},
        os::fd::AsRawFd,
        thread,
        time::Instant as StdInstant,
    };

    let mut output = stdout();
    output.write_all(b"\x1b]11;?\x1b\\").ok()?;
    output.flush().ok()?;

    let mut input = std::io::stdin();
    let fd = input.as_raw_fd();
    let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if original_flags < 0 {
        return None;
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } < 0 {
        return None;
    }

    let _restore_flags = scopeguard::guard((), |_| unsafe {
        libc::fcntl(fd, libc::F_SETFL, original_flags);
    });

    let started = StdInstant::now();
    let mut response = Vec::new();
    let mut buffer = [0_u8; 64];

    while started.elapsed() < Duration::from_millis(160) {
        match input.read(&mut buffer) {
            Ok(0) => thread::sleep(Duration::from_millis(2)),
            Ok(read) => {
                response.extend_from_slice(&buffer[..read]);
                if response.ends_with(b"\x07") || response.ends_with(b"\x1b\\") {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return None,
        }
    }

    let (red, green, blue) = parse_terminal_background_response(&response)?;
    if perceived_luminance(red, green, blue) > 160.0 {
        Some(TerminalTheme::Light)
    } else {
        Some(TerminalTheme::Dark)
    }
}

#[cfg(not(unix))]
fn query_terminal_background() -> Option<TerminalTheme> {
    None
}

fn parse_terminal_background_response(response: &[u8]) -> Option<(u8, u8, u8)> {
    let response = std::str::from_utf8(response).ok()?;
    let color_start = response
        .find("]11;rgba:")
        .map(|idx| idx + "]11;rgba:".len())
        .or_else(|| response.find("]11;rgb:").map(|idx| idx + "]11;rgb:".len()))?;
    let color = &response[color_start..];
    let color = color.split(['\x07', '\x1b']).next()?;
    let mut components = color.split('/');

    Some((
        parse_terminal_color_component(components.next()?)?,
        parse_terminal_color_component(components.next()?)?,
        parse_terminal_color_component(components.next()?)?,
    ))
}

fn parse_terminal_color_component(component: &str) -> Option<u8> {
    let digits: String = component
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .take(4)
        .collect();
    if digits.is_empty() {
        return None;
    }

    let value = u32::from_str_radix(&digits, 16).ok()?;
    let max = (1_u32 << (digits.len() * 4)) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

fn perceived_luminance(red: u8, green: u8, blue: u8) -> f64 {
    (red as f64 * 0.299) + (green as f64 * 0.587) + (blue as f64 * 0.114)
}

fn template_list_item(
    template: &TemplateSearchItem,
    selected: bool,
    theme: TerminalTheme,
) -> ListItem<'static> {
    let description = template
        .description
        .clone()
        .unwrap_or_else(|| "No description".to_string());
    let creator = template
        .creator_name
        .clone()
        .unwrap_or_else(|| "Unknown creator".to_string());

    if selected {
        return ListItem::new(vec![
            Line::raw(""),
            Line::from(vec![
                Span::raw(RESULT_PADDING),
                Span::styled(template.name.clone(), template_name_style(selected, theme)),
            ]),
            Line::from(vec![
                Span::raw(RESULT_PADDING),
                Span::styled(
                    truncate_chars(&description, 92),
                    muted_style(selected, theme),
                ),
            ]),
            Line::from(metadata_spans(template, &creator, selected, theme)),
            Line::raw(""),
        ])
        .style(Style::default().bg(selected_background(theme)));
    }

    ListItem::new(vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw(RESULT_PADDING),
            Span::styled(template.name.clone(), template_name_style(selected, theme)),
        ]),
        Line::from(vec![
            Span::raw(RESULT_PADDING),
            Span::styled(
                truncate_chars(&description, 92),
                muted_style(selected, theme),
            ),
        ]),
        Line::from(metadata_spans(template, &creator, selected, theme)),
        Line::raw(""),
    ])
}

fn metadata_spans(
    template: &TemplateSearchItem,
    creator: &str,
    selected: bool,
    theme: TerminalTheme,
) -> Vec<Span<'static>> {
    let muted = muted_style(selected, theme);
    let health_style = template
        .health_score
        .map(health_color)
        .map(|color| Style::default().fg(color))
        .unwrap_or(muted);

    let mut spans = vec![
        Span::raw(RESULT_PADDING),
        Span::styled("↓ ", muted),
        Span::styled(format_count(template.deployment_count), muted),
        Span::styled(" • ", muted),
        Span::styled("∿ ", health_style),
        Span::styled(format_health(template.health_score), health_style),
        Span::styled(" • by ", muted),
        Span::styled(creator.to_string(), muted),
    ];

    if template.is_verified {
        spans.push(Span::styled(" • ", muted));
        spans.push(Span::styled(
            "✓ verified",
            Style::default().fg(Color::Green),
        ));
    }

    spans
}

fn selected_background(theme: TerminalTheme) -> Color {
    match theme {
        TerminalTheme::Dark => Color::Indexed(236),
        TerminalTheme::Light => Color::Indexed(255),
    }
}

fn template_name_style(selected: bool, theme: TerminalTheme) -> Style {
    let style = Style::default().add_modifier(Modifier::BOLD);
    if !selected {
        return style;
    }

    match theme {
        TerminalTheme::Dark => style.fg(Color::White),
        TerminalTheme::Light => style.fg(Color::Black),
    }
}

fn muted_style(selected: bool, theme: TerminalTheme) -> Style {
    if !selected {
        return Style::default().fg(Color::DarkGray);
    }

    match theme {
        TerminalTheme::Dark => Style::default().fg(Color::Gray),
        TerminalTheme::Light => Style::default().fg(Color::DarkGray),
    }
}

fn print_template_results(request: &TemplateSearchRequest, connection: &TemplateSearchConnection) {
    if connection.edges.is_empty() {
        if request.query.is_empty() {
            println!("No templates found.");
        } else {
            println!("No templates found matching '{}'.", request.query);
        }
        return;
    }

    if request.query.is_empty() {
        println!("Templates:");
    } else {
        println!("Templates matching '{}':", request.query);
    }

    for edge in &connection.edges {
        let template = &edge.node;
        println!();
        println!("{} ({})", template.name, template.code);
        if let Some(description) = &template.description {
            println!("  {}", truncate_chars(description, 100));
        }
        println!(
            "  deploys {} | health {} | by {}{}",
            format_count(template.deployment_count),
            format_health(template.health_score),
            template
                .creator_name
                .as_deref()
                .unwrap_or("Unknown creator"),
            if template.is_verified {
                " | verified"
            } else {
                ""
            }
        );
    }

    if connection.page_info.has_next_page {
        if let Some(cursor) = &connection.page_info.end_cursor {
            println!();
            println!("Next page cursor: {cursor}");
            println!("Next page command:");
            println!("  {}", next_page_command(request, cursor));
        }
    }
}

fn print_selected_template(template: &TemplateSearchItem) {
    println!("{} ({})", template.name, template.code);
    if let Some(description) = &template.description {
        println!("{description}");
    }
    println!();
    println!("Deploy with:");
    println!("  railway deploy --template {}", template.code);
}

fn next_page_command(request: &TemplateSearchRequest, cursor: &str) -> String {
    let mut parts = vec![
        "railway".to_string(),
        "templates".to_string(),
        "search".to_string(),
    ];
    if !request.query.is_empty() {
        parts.push(shell_arg(&request.query));
    }
    parts.push("--limit".to_string());
    parts.push(request.limit.to_string());
    parts.push("--after".to_string());
    parts.push(shell_arg(cursor));
    if let Some(category) = &request.category {
        parts.push("--category".to_string());
        parts.push(shell_arg(category));
    }
    if let Some(verified) = request.verified {
        parts.push("--verified".to_string());
        parts.push(verified.to_string());
    }
    parts.join(" ")
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn format_health(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.0}%"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn health_color(value: f64) -> Color {
    if value > 75.0 {
        Color::Green
    } else if value > 50.0 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn format_count(value: i64) -> String {
    let value = value.to_string();
    let mut output = String::new();
    for (idx, ch) in value.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            output.push(',');
        }
        output.push(ch);
    }
    output.chars().rev().collect()
}

fn truncate_chars(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpublish_guard_accepts_published_templates() {
        assert!(
            ensure_template_can_unpublish(&queries::template::TemplateStatus::PUBLISHED, "App")
                .is_ok()
        );
    }

    #[test]
    fn unpublish_guard_rejects_unpublished_templates() {
        let error = ensure_template_can_unpublish(
            &queries::template::TemplateStatus::UNPUBLISHED,
            "Draft App",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("Template \"Draft App\" is UNPUBLISHED"));
        assert!(error.contains("Only published templates can be unpublished"));
    }

    #[test]
    fn unpublish_guard_rejects_hidden_templates() {
        let error =
            ensure_template_can_unpublish(&queries::template::TemplateStatus::HIDDEN, "Hidden App")
                .unwrap_err()
                .to_string();

        assert!(error.contains("Template \"Hidden App\" is HIDDEN"));
        assert!(error.contains("Only published templates can be unpublished"));
    }

    #[test]
    fn optional_publish_field_treats_explicit_empty_as_clear() {
        assert_eq!(
            resolve_optional_publish_field(
                Some("".to_string()),
                Some("https://example.com/image.png".to_string()),
                false,
                "Image URL",
                "<none>",
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn optional_publish_field_treats_clear_sentinel_as_clear() {
        assert_eq!(
            resolve_optional_publish_field(
                Some(" none ".to_string()),
                Some("https://example.com/image.png".to_string()),
                false,
                "Image URL",
                "<none>",
            )
            .unwrap(),
            None
        );
        assert_eq!(
            resolve_optional_publish_field(
                Some("CLEAR".to_string()),
                Some("demo-project-id".to_string()),
                false,
                "Public demo project ID",
                "<none>",
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn optional_publish_field_keeps_existing_when_unspecified() {
        assert_eq!(
            resolve_optional_publish_field(
                None,
                Some("demo-project-id".to_string()),
                false,
                "Public demo project ID",
                "<none>",
            )
            .unwrap(),
            Some("demo-project-id".to_string())
        );
    }

    #[test]
    fn template_publish_variables_serialize_clearable_fields_as_null() {
        let value = serde_json::to_value(mutations::template_publish::Variables {
            id: "template-id".to_string(),
            description: "Deploy and Host Test App".to_string(),
            category: "Other".to_string(),
            readme: default_readme("Test App"),
            image: None,
            demo_project_id: None,
            workspace_id: None,
        })
        .unwrap();

        assert!(value.get("image").is_some_and(serde_json::Value::is_null));
        assert!(
            value
                .get("demoProjectId")
                .is_some_and(serde_json::Value::is_null)
        );
        assert!(
            value
                .get("workspaceId")
                .is_some_and(serde_json::Value::is_null)
        );
    }
}
