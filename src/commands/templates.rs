use std::{
    env,
    io::stdout,
    panic,
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

use crate::{client::post_graphql, consts::TICK_STRING};

use super::*;

type TemplateSearchResponse = queries::template_search::ResponseData;
type TemplateSearchConnection = queries::template_search::TemplateSearchTemplateSearch;
type TemplateSearchEdge = queries::template_search::TemplateSearchTemplateSearchEdges;
type TemplateSearchItem = queries::template_search::TemplateSearchTemplateSearchEdgesNode;

const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 50;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const RESULT_PADDING: &str = "  ";

#[derive(Clone, Copy)]
enum TerminalTheme {
    Dark,
    Light,
}

/// Discover Railway templates
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway templates search postgres --json\n  railway template find redis --limit 5 --json\n  railway templates ls --category database --json"
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
    }
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
