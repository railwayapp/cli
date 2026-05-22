mod data;

use std::io::stdout;
use std::panic;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use self::data::{ProjectCard, load_project_cards};

const PROJECT_CARD_MIN_WIDTH: u16 = 30;
const PROJECT_CARD_HEIGHT: u16 = 7;
const PROJECT_CARD_GAP: u16 = 1;
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DashboardAuthMode {
    Workspace,
    LinkedProject {
        project_id: String,
        environment_id: String,
    },
}

#[derive(Clone, Debug)]
pub struct DashTuiParams {
    pub project: Option<String>,
    pub environment: Option<String>,
    pub auth_mode: DashboardAuthMode,
}

#[derive(Clone, Debug)]
struct DashApp {
    params: DashTuiParams,
    screen: DashboardScreen,
    project_preview: Option<ProjectCard>,
    spinner_tick: usize,
}

#[derive(Clone, Debug)]
enum DashboardScreen {
    Projects(ProjectsScreenState),
    LinkedProjectPlaceholder,
}

#[derive(Clone, Debug)]
struct ProjectsScreenState {
    cards: Vec<ProjectCard>,
    selected: usize,
    filter: String,
    filter_mode: bool,
    loading: bool,
    error: Option<String>,
    current_request_id: u64,
    initial_selection_hint: Option<String>,
}

enum LoaderEvent {
    ProjectsLoaded {
        request_id: u64,
        result: std::result::Result<Vec<ProjectCard>, String>,
    },
}

impl DashApp {
    fn new(params: DashTuiParams) -> Self {
        let screen = match params.auth_mode {
            DashboardAuthMode::Workspace => {
                DashboardScreen::Projects(ProjectsScreenState::new(params.project.clone()))
            }
            DashboardAuthMode::LinkedProject { .. } => DashboardScreen::LinkedProjectPlaceholder,
        };

        Self {
            params,
            screen,
            project_preview: None,
            spinner_tick: 0,
        }
    }

    fn start_initial_load(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        if matches!(self.screen, DashboardScreen::Projects(_)) {
            self.refresh_projects(tx);
        }
    }

    fn refresh_projects(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        let DashboardScreen::Projects(state) = &mut self.screen else {
            return;
        };

        state.loading = true;
        state.error = None;
        state.current_request_id += 1;
        let request_id = state.current_request_id;
        let tx = tx.clone();

        tokio::spawn(async move {
            let result = load_project_cards()
                .await
                .map_err(|error| error.to_string());
            let _ = tx.send(LoaderEvent::ProjectsLoaded { request_id, result });
        });
    }

    fn handle_loader_event(&mut self, event: LoaderEvent) {
        match event {
            LoaderEvent::ProjectsLoaded { request_id, result } => {
                let DashboardScreen::Projects(state) = &mut self.screen else {
                    return;
                };

                if request_id != state.current_request_id {
                    return;
                }

                match result {
                    Ok(cards) => state.apply_loaded_cards(cards),
                    Err(error) => state.set_error(error),
                }
            }
        }
    }

    fn handle_event(
        &mut self,
        event: Event,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
        tx: &mpsc::UnboundedSender<LoaderEvent>,
    ) -> bool {
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.handle_key(key, terminal.size().unwrap_or_default().width, tx)
            }
            Event::Resize(_, _) => {
                let _ = terminal.clear();
                false
            }
            _ => false,
        }
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        terminal_width: u16,
        tx: &mpsc::UnboundedSender<LoaderEvent>,
    ) -> bool {
        if matches!(key.code, KeyCode::Char('q'))
            || (matches!(key.code, KeyCode::Char('c'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return true;
        }

        if self.project_preview.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Backspace) {
                self.project_preview = None;
            }
            return false;
        }

        match &mut self.screen {
            DashboardScreen::Projects(state) => {
                if state.filter_mode {
                    handle_projects_filter_input(state, key);
                    return false;
                }

                let columns = project_grid_columns(terminal_width.saturating_sub(4));
                match key.code {
                    KeyCode::Up | KeyCode::Char('i') => state.move_up(columns),
                    KeyCode::Down | KeyCode::Char('k') => state.move_down(columns),
                    KeyCode::Left | KeyCode::Char('j') => state.move_left(),
                    KeyCode::Right | KeyCode::Char('l') => state.move_right(),
                    KeyCode::Enter => {
                        self.project_preview = state.selected_card().cloned();
                    }
                    KeyCode::Char('/') => state.filter_mode = true,
                    KeyCode::Char('r') => self.refresh_projects(tx),
                    _ => {}
                }
            }
            DashboardScreen::LinkedProjectPlaceholder => {}
        }

        false
    }

    fn on_tick(&mut self) {
        self.spinner_tick = (self.spinner_tick + 1) % SPINNER_FRAMES.len();
    }
}

impl ProjectsScreenState {
    fn new(initial_selection_hint: Option<String>) -> Self {
        Self {
            cards: Vec::new(),
            selected: 0,
            filter: String::new(),
            filter_mode: false,
            loading: false,
            error: None,
            current_request_id: 0,
            initial_selection_hint,
        }
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.cards
            .iter()
            .enumerate()
            .filter_map(|(index, card)| card.matches_filter(&self.filter).then_some(index))
            .collect()
    }

    fn selected_card(&self) -> Option<&ProjectCard> {
        let visible = self.visible_indices();
        visible
            .get(self.selected)
            .and_then(|index| self.cards.get(*index))
    }

    fn move_left(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn move_right(&mut self) {
        let visible_len = self.visible_indices().len();
        if visible_len > 0 {
            self.selected = (self.selected + 1).min(visible_len - 1);
        }
    }

    fn move_up(&mut self, columns: usize) {
        self.selected = self.selected.saturating_sub(columns.max(1));
    }

    fn move_down(&mut self, columns: usize) {
        let visible_len = self.visible_indices().len();
        if visible_len > 0 {
            self.selected = (self.selected + columns.max(1)).min(visible_len - 1);
        }
    }

    fn apply_loaded_cards(&mut self, cards: Vec<ProjectCard>) {
        let preferred_id = self
            .selected_card()
            .map(|card| card.id.clone())
            .or_else(|| self.initial_selection_hint.clone());

        self.cards = cards;
        self.loading = false;
        self.error = None;
        self.clamp_selection();

        if let Some(preferred_id) = preferred_id {
            self.select_by_project_id(&preferred_id);
        }

        self.initial_selection_hint = None;
    }

    fn set_error(&mut self, error: String) {
        self.loading = false;
        self.error = Some(error);
        self.clamp_selection();
    }

    fn select_by_project_id(&mut self, project_id: &str) {
        let visible = self.visible_indices();
        if let Some(position) = visible
            .iter()
            .position(|index| self.cards[*index].id == project_id)
        {
            self.selected = position;
        } else {
            self.clamp_selection();
        }
    }

    fn clamp_selection(&mut self) {
        let visible_len = self.visible_indices().len();
        if visible_len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(visible_len - 1);
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;

    let rollback = scopeguard::guard((), |_| {
        restore_terminal();
    });

    execute!(stdout(), EnterAlternateScreen, Hide, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    std::mem::forget(rollback);

    Ok(terminal)
}

fn restore_terminal() {
    let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

pub async fn run(params: DashTuiParams) -> Result<()> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let _cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    let mut app = DashApp::new(params);
    let mut events = EventStream::new();
    let (loader_tx, mut loader_rx) = mpsc::unbounded_channel();
    app.start_initial_load(&loader_tx);

    let mut tick = tokio::time::interval(Duration::from_millis(120));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| render(frame, &app))?;

        tokio::select! {
            Some(Ok(event)) = events.next() => {
                if app.handle_event(event, &mut terminal, &loader_tx) {
                    break;
                }
            }
            Some(loader_event) = loader_rx.recv() => {
                app.handle_loader_event(loader_event);
            }
            _ = tick.tick() => {
                app.on_tick();
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }

    Ok(())
}

fn handle_projects_filter_input(state: &mut ProjectsScreenState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => state.filter_mode = false,
        KeyCode::Enter => state.filter_mode = false,
        KeyCode::Backspace => {
            state.filter.pop();
            state.clamp_selection();
        }
        KeyCode::Delete => {
            state.filter.clear();
            state.selected = 0;
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.filter.clear();
            state.selected = 0;
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.filter.push(ch);
            state.selected = 0;
        }
        _ => {}
    }
}

fn render(frame: &mut Frame<'_>, app: &DashApp) {
    let area = frame.area();
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(3),
    ])
    .areas(area);

    render_header(frame, header, app);

    match &app.screen {
        DashboardScreen::Projects(state) => {
            render_projects_screen(frame, body, app, state);
            if let Some(card) = &app.project_preview {
                render_project_preview(frame, centered_rect(body, 70, 65), card);
            }
        }
        DashboardScreen::LinkedProjectPlaceholder => render_linked_project_placeholder(
            frame,
            body,
            app.params.project.as_deref().unwrap_or("(linked project)"),
            app.params
                .environment
                .as_deref()
                .unwrap_or("(linked/default environment)"),
            match &app.params.auth_mode {
                DashboardAuthMode::LinkedProject {
                    project_id,
                    environment_id: _,
                } => project_id,
                DashboardAuthMode::Workspace => unreachable!(),
            },
            match &app.params.auth_mode {
                DashboardAuthMode::LinkedProject {
                    project_id: _,
                    environment_id,
                } => environment_id,
                DashboardAuthMode::Workspace => unreachable!(),
            },
        ),
    }

    render_footer(frame, footer, app);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &DashApp) {
    let subtitle = if app.project_preview.is_some() {
        "project preview placeholder"
    } else {
        match &app.screen {
            DashboardScreen::Projects(_) => "project cards",
            DashboardScreen::LinkedProjectPlaceholder => "linked project placeholder",
        }
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Railway Dashboard ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(subtitle),
        ]))
        .block(Block::default().borders(Borders::ALL).title("railway dash")),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &DashApp) {
    let text = if app.project_preview.is_some() {
        "Esc back • q quit"
    } else {
        match &app.screen {
            DashboardScreen::Projects(state) if state.filter_mode => {
                "type to filter • Enter apply • Esc close • Backspace delete • q quit"
            }
            DashboardScreen::Projects(_) => {
                "Enter open • arrows/ijkl move • / filter • r reload • q quit"
            }
            DashboardScreen::LinkedProjectPlaceholder => {
                "q quit • Ctrl-C quit • linked project overview coming next"
            }
        }
    };

    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("controls"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_projects_screen(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &DashApp,
    state: &ProjectsScreenState,
) {
    frame.render_widget(Clear, area);

    let [status_area, grid_area] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(8)]).areas(area);

    render_projects_status(frame, status_area, app, state);
    render_projects_grid(frame, grid_area, state);
}

fn render_projects_status(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &DashApp,
    state: &ProjectsScreenState,
) {
    let selected = state
        .selected_card()
        .map(|card| card.name.as_str())
        .unwrap_or("none");
    let visible_count = state.visible_indices().len();
    let spinner = SPINNER_FRAMES[app.spinner_tick % SPINNER_FRAMES.len()];

    let filter_line = if state.filter_mode {
        Line::from(vec![
            Span::styled("filter: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{}█", state.filter),
                Style::default().fg(Color::Yellow),
            ),
        ])
    } else if state.filter.is_empty() {
        Line::from(vec![
            Span::styled("filter: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("/ to search projects"),
        ])
    } else {
        Line::from(vec![
            Span::styled("filter: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(state.filter.as_str(), Style::default().fg(Color::Yellow)),
        ])
    };

    let status_line = if let Some(error) = &state.error {
        Line::from(vec![
            Span::styled(
                "error: ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(error),
        ])
    } else if state.loading {
        let label = if state.cards.is_empty() {
            "Loading projects..."
        } else {
            "Refreshing projects..."
        };
        Line::from(vec![
            Span::styled(format!("{spinner} "), Style::default().fg(Color::Cyan)),
            Span::raw(label),
        ])
    } else {
        Line::from(vec![
            Span::styled("selected: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(selected),
            Span::raw("  •  "),
            Span::styled(
                format!("{visible_count} visible / {} total", state.cards.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };

    frame.render_widget(
        Paragraph::new(vec![filter_line, Line::default(), status_line])
            .block(Block::default().borders(Borders::ALL).title("projects"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_projects_grid(frame: &mut Frame<'_>, area: Rect, state: &ProjectsScreenState) {
    let block = Block::default().borders(Borders::ALL).title("cards");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < PROJECT_CARD_MIN_WIDTH || inner.height < PROJECT_CARD_HEIGHT {
        frame.render_widget(
            Paragraph::new("Terminal too small for project cards. Resize to continue.")
                .style(Style::default().fg(Color::Yellow))
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let visible = state.visible_indices();
    if state.loading && state.cards.is_empty() {
        frame.render_widget(
            Paragraph::new("Loading projects...")
                .style(Style::default().fg(Color::Cyan))
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    if let Some(error) = &state.error
        && state.cards.is_empty()
    {
        frame.render_widget(
            Paragraph::new(format!(
                "Unable to load projects.\n\n{error}\n\nPress r to retry."
            ))
            .style(Style::default().fg(Color::Red))
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    if visible.is_empty() {
        let message = if state.filter.is_empty() {
            "No projects found for this account."
        } else {
            "No projects match the current filter."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let columns = project_grid_columns(inner.width).max(1);
    let card_width = project_card_width(inner.width, columns);
    let rows_per_page = project_rows_per_page(inner.height).max(1);
    let selected_row = state.selected / columns;
    let start_row = selected_row.saturating_sub(rows_per_page.saturating_sub(1));
    let start_index = start_row * columns;
    let end_index = (start_index + (rows_per_page * columns)).min(visible.len());

    for (visible_index, card_index) in visible[start_index..end_index].iter().enumerate() {
        let absolute_visible_index = start_index + visible_index;
        let card = &state.cards[*card_index];
        let row = visible_index / columns;
        let col = visible_index % columns;
        let x = inner.x + (col as u16 * (card_width + PROJECT_CARD_GAP));
        let y = inner.y + (row as u16 * (PROJECT_CARD_HEIGHT + PROJECT_CARD_GAP));
        let rect = Rect {
            x,
            y,
            width: card_width,
            height: PROJECT_CARD_HEIGHT,
        };
        render_project_card(frame, rect, card, absolute_visible_index == state.selected);
    }
}

fn render_project_card(frame: &mut Frame<'_>, area: Rect, card: &ProjectCard, selected: bool) {
    let border_style = if selected {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title_style = if selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };

    let workspace_name = card.workspace_name.as_deref().unwrap_or("personal");
    let service_label = pluralize(card.service_count, "service");
    let environment_label = pluralize(card.environment_count, "environment");

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(card.name.as_str(), title_style)),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    format!("{} ", card.service_count),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(service_label),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("{} ", card.environment_count),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(environment_label),
            ]),
            Line::from(vec![
                Span::styled("workspace: ", Style::default().fg(Color::DarkGray)),
                Span::raw(workspace_name),
            ]),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(card.id.as_str()),
        )
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_project_preview(frame: &mut Frame<'_>, area: Rect, card: &ProjectCard) {
    let workspace_name = card.workspace_name.as_deref().unwrap_or("personal");

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Project overview is the next phase.",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from(vec![
                Span::styled("project: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(card.name.as_str()),
            ]),
            Line::from(vec![
                Span::styled(
                    "project id: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(card.id.as_str()),
            ]),
            Line::from(vec![
                Span::styled("workspace: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(workspace_name),
            ]),
            Line::from(vec![
                Span::styled("services: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(card.service_count.to_string()),
            ]),
            Line::from(vec![
                Span::styled(
                    "environments: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(card.environment_count.to_string()),
            ]),
            Line::default(),
            Line::from("Phase 4 will open this project and resolve its selected environment."),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("project preview"),
        )
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_linked_project_placeholder(
    frame: &mut Frame<'_>,
    area: Rect,
    requested_project: &str,
    requested_environment: &str,
    project_id: &str,
    environment_id: &str,
) {
    let content = vec![
        Line::from(Span::styled(
            "Dashboard shell is wired up.",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from("This placeholder was opened through project-scoped auth preflight."),
        Line::from("Later phases will route straight into the linked project overview."),
        Line::default(),
        Line::from(vec![
            Span::styled(
                "entry mode: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("project-scoped auth → linked project overview"),
        ]),
        Line::from(vec![
            Span::styled(
                "validated project id: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(project_id),
        ]),
        Line::from(vec![
            Span::styled(
                "validated environment id: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(environment_id),
        ]),
        Line::default(),
        Line::from(vec![
            Span::styled(
                "requested project: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(requested_project),
        ]),
        Line::from(vec![
            Span::styled(
                "requested environment: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(requested_environment),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(content)
            .block(Block::default().borders(Borders::ALL).title("placeholder"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn project_grid_columns(width: u16) -> usize {
    let stride = PROJECT_CARD_MIN_WIDTH + PROJECT_CARD_GAP;
    ((width + PROJECT_CARD_GAP) / stride).max(1) as usize
}

fn project_card_width(width: u16, columns: usize) -> u16 {
    let columns = columns.max(1) as u16;
    let total_gaps = PROJECT_CARD_GAP.saturating_mul(columns.saturating_sub(1));
    width.saturating_sub(total_gaps) / columns
}

fn project_rows_per_page(height: u16) -> usize {
    let stride = PROJECT_CARD_HEIGHT + PROJECT_CARD_GAP;
    ((height + PROJECT_CARD_GAP) / stride).max(1) as usize
}

fn pluralize(count: usize, singular: &str) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        format!("{singular}s")
    }
}

fn centered_rect(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - height_percent) / 2),
        Constraint::Percentage(height_percent),
        Constraint::Percentage((100 - height_percent) / 2),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - width_percent) / 2),
        Constraint::Percentage(width_percent),
        Constraint::Percentage((100 - width_percent) / 2),
    ])
    .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, name: &str) -> ProjectCard {
        ProjectCard {
            id: id.to_string(),
            name: name.to_string(),
            workspace_name: Some("workspace".to_string()),
            service_count: 2,
            environment_count: 3,
        }
    }

    #[test]
    fn refresh_preserves_selection_by_project_id() {
        let mut state = ProjectsScreenState::new(None);
        state.cards = vec![card("one", "one"), card("two", "two")];
        state.selected = 1;

        state.apply_loaded_cards(vec![card("zero", "zero"), card("two", "two")]);

        assert_eq!(
            state.selected_card().map(|card| card.id.as_str()),
            Some("two")
        );
    }

    #[test]
    fn initial_selection_hint_is_applied_after_first_load() {
        let mut state = ProjectsScreenState::new(Some("proj_two".to_string()));
        state.apply_loaded_cards(vec![card("proj_one", "one"), card("proj_two", "two")]);

        assert_eq!(
            state.selected_card().map(|card| card.id.as_str()),
            Some("proj_two")
        );
    }

    #[test]
    fn project_grid_has_at_least_one_column() {
        assert_eq!(project_grid_columns(10), 1);
        assert!(project_grid_columns(120) >= 1);
    }
}
