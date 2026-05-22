use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::{DashApp, DashboardAuthMode, DashboardScreen, ProjectScreenState, ProjectsScreenState};
use crate::controllers::dash_tui::data::{DashboardService, ProjectCard};

pub(super) const PROJECT_CARD_MIN_WIDTH: u16 = 30;
pub(super) const PROJECT_CARD_HEIGHT: u16 = 7;
pub(super) const PROJECT_CARD_GAP: u16 = 1;
pub(super) const SERVICE_CARD_MIN_WIDTH: u16 = 26;
pub(super) const SERVICE_CARD_HEIGHT: u16 = 7;
pub(super) const SERVICE_CARD_GAP: u16 = 1;

const RAILWAY_VIOLET: Color = Color::Rgb(127, 86, 217);
const RAILWAY_PURPLE: Color = Color::Rgb(155, 107, 255);
const RAILWAY_PINK: Color = Color::Rgb(236, 72, 153);
const RAILWAY_LAVENDER: Color = Color::Rgb(221, 214, 254);
const RAILWAY_MUTED: Color = Color::Rgb(161, 152, 190);
const RAILWAY_PANEL: Color = Color::Rgb(91, 78, 129);
const RAILWAY_ERROR: Color = Color::Rgb(248, 113, 113);

fn panel_block<'a>(title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(panel_border_style())
        .title(Span::styled(title, panel_title_style()))
}

fn panel_border_style() -> Style {
    Style::default().fg(RAILWAY_PANEL)
}

fn panel_title_style() -> Style {
    Style::default()
        .fg(RAILWAY_PURPLE)
        .add_modifier(Modifier::BOLD)
}

fn hero_style() -> Style {
    Style::default()
        .fg(RAILWAY_LAVENDER)
        .add_modifier(Modifier::BOLD)
}

fn selected_border_style() -> Style {
    Style::default().fg(RAILWAY_PINK)
}

fn selected_title_style() -> Style {
    Style::default()
        .fg(RAILWAY_PINK)
        .add_modifier(Modifier::BOLD)
}

fn loading_style() -> Style {
    Style::default().fg(RAILWAY_VIOLET)
}

fn accent_style() -> Style {
    Style::default().fg(RAILWAY_PINK)
}

fn muted_style() -> Style {
    Style::default().fg(RAILWAY_MUTED)
}

fn error_style() -> Style {
    Style::default()
        .fg(RAILWAY_ERROR)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn render(frame: &mut Frame<'_>, app: &DashApp) {
    let [header, body, footer] = dashboard_sections(frame.area());

    render_header(frame, header, app);

    match &app.screen {
        DashboardScreen::Projects(state) => render_projects_screen(frame, body, app, state),
        DashboardScreen::Project(state) => render_project_screen(frame, body, app, state),
    }

    render_footer(frame, footer, app);
}

pub(super) fn project_navigation_columns(frame_area: Rect) -> usize {
    project_grid_columns_for_area(projects_grid_area(frame_area))
}

pub(super) fn service_navigation_columns(frame_area: Rect) -> usize {
    service_grid_columns_for_area(project_services_area(frame_area))
}

pub(super) fn project_grid_columns(width: u16) -> usize {
    let stride = PROJECT_CARD_MIN_WIDTH + PROJECT_CARD_GAP;
    ((width + PROJECT_CARD_GAP) / stride).max(1) as usize
}

pub(super) fn service_grid_columns(width: u16) -> usize {
    let stride = SERVICE_CARD_MIN_WIDTH + SERVICE_CARD_GAP;
    ((width + SERVICE_CARD_GAP) / stride).max(1) as usize
}

fn project_grid_columns_for_area(area: Rect) -> usize {
    project_grid_columns(panel_inner_area(area).width).max(1)
}

fn service_grid_columns_for_area(area: Rect) -> usize {
    service_grid_columns(panel_inner_area(area).width).max(1)
}

fn dashboard_sections(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(3),
    ])
    .areas(area)
}

fn projects_sections(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Length(4), Constraint::Min(8)]).areas(area)
}

fn project_sections(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Length(4), Constraint::Min(8)]).areas(area)
}

fn project_overview_sections(area: Rect) -> [Rect; 2] {
    Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)]).areas(area)
}

fn projects_grid_area(frame_area: Rect) -> Rect {
    let [_, body, _] = dashboard_sections(frame_area);
    let [_, grid_area] = projects_sections(body);
    grid_area
}

fn project_services_area(frame_area: Rect) -> Rect {
    let [_, body, _] = dashboard_sections(frame_area);
    let [_, main_area] = project_sections(body);
    let [diagram_area, _] = project_overview_sections(main_area);
    diagram_area
}

fn panel_inner_area(area: Rect) -> Rect {
    panel_block("").inner(area)
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &DashApp) {
    let subtitle = match &app.screen {
        DashboardScreen::Projects(state) if state.filter_mode => "project cards • filtering",
        DashboardScreen::Projects(_) => "project cards",
        DashboardScreen::Project(state) if state.environment_selector.is_some() => {
            "project overview • environment selector"
        }
        DashboardScreen::Project(_) => "project overview",
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Railway Dashboard ", hero_style()),
            Span::styled(subtitle, Style::default().fg(RAILWAY_LAVENDER)),
        ]))
        .block(panel_block("railway dash")),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &DashApp) {
    let text = match &app.screen {
        DashboardScreen::Projects(state) if state.filter_mode => {
            "type to filter • Enter apply • Esc close • Backspace delete • q quit"
        }
        DashboardScreen::Projects(_) => {
            "Enter open • arrows/ijkl move • / filter • r reload • q quit"
        }
        DashboardScreen::Project(state) if state.environment_selector.is_some() => {
            "Enter switch environment • arrows/ik move • Esc cancel • q quit"
        }
        DashboardScreen::Project(_) => match app.params.auth_mode {
            DashboardAuthMode::Workspace => {
                "Esc back • e environments • arrows/ijkl move • r reload • q quit"
            }
            DashboardAuthMode::LinkedProject { .. } => {
                "e environments • arrows/ijkl move • r reload • q quit"
            }
        },
    };

    frame.render_widget(
        Paragraph::new(text)
            .style(muted_style())
            .block(panel_block("controls"))
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

    let [status_area, grid_area] = projects_sections(area);

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
    let spinner = super::SPINNER_FRAMES[app.spinner_tick % super::SPINNER_FRAMES.len()];

    let filter_line = if state.filter_mode {
        Line::from(vec![
            Span::styled("filter: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("{}█", state.filter), selected_title_style()),
        ])
    } else if state.filter.is_empty() {
        Line::from(vec![
            Span::styled("filter: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("/ to search projects", muted_style()),
        ])
    } else {
        Line::from(vec![
            Span::styled("filter: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(state.filter.as_str(), selected_title_style()),
        ])
    };

    let status_line = if let Some(error) = &state.error {
        Line::from(vec![
            Span::styled("error: ", error_style()),
            Span::raw(error),
        ])
    } else if state.loading {
        let label = if state.cards.is_empty() {
            "Loading projects..."
        } else {
            "Refreshing projects..."
        };
        Line::from(vec![
            Span::styled(format!("{spinner} "), loading_style()),
            Span::styled(label, Style::default().fg(RAILWAY_LAVENDER)),
        ])
    } else {
        Line::from(vec![
            Span::styled("selected: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(selected, Style::default().fg(RAILWAY_LAVENDER)),
            Span::raw("  •  "),
            Span::styled(
                format!("{visible_count} visible / {} total", state.cards.len()),
                muted_style(),
            ),
        ])
    };

    frame.render_widget(
        Paragraph::new(vec![filter_line, Line::default(), status_line])
            .block(panel_block("projects"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_projects_grid(frame: &mut Frame<'_>, area: Rect, state: &ProjectsScreenState) {
    let block = panel_block("cards");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < PROJECT_CARD_MIN_WIDTH || inner.height < PROJECT_CARD_HEIGHT {
        frame.render_widget(
            Paragraph::new("Terminal too small for project cards. Resize to continue.")
                .style(accent_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let visible = state.visible_indices();
    if state.loading && state.cards.is_empty() {
        frame.render_widget(
            Paragraph::new("Loading projects...")
                .style(loading_style())
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
            .style(error_style())
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
                .style(muted_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let columns = project_grid_columns_for_area(area);
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
        selected_border_style()
    } else {
        panel_border_style()
    };
    let title_style = if selected {
        selected_title_style()
    } else {
        hero_style()
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
                Span::styled("workspace: ", muted_style()),
                Span::styled(workspace_name, Style::default().fg(RAILWAY_LAVENDER)),
            ]),
        ])
        .block(panel_block(card.id.as_str()).border_style(border_style))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_project_screen(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &DashApp,
    state: &ProjectScreenState,
) {
    frame.render_widget(Clear, area);

    let [status_area, main_area] = project_sections(area);

    render_project_status(frame, status_area, app, state);

    if state.loading && state.project.is_none() {
        render_centered_message(
            frame,
            main_area,
            &format!(
                "{} Loading project overview...",
                super::SPINNER_FRAMES[app.spinner_tick % super::SPINNER_FRAMES.len()]
            ),
            RAILWAY_VIOLET,
            "project overview",
        );
        return;
    }

    if let Some(error) = &state.error
        && state.project.is_none()
    {
        render_centered_message(
            frame,
            main_area,
            &format!("Unable to open project.\n\n{error}\n\nPress r to retry."),
            RAILWAY_ERROR,
            "project overview",
        );
        return;
    }

    let [diagram_area, summary_area] = project_overview_sections(main_area);

    render_project_diagram(frame, diagram_area, state);
    render_project_summary(frame, summary_area, state);

    if state.environment_selector.is_some() {
        render_environment_selector(frame, centered_rect(main_area, 55, 60), state);
    }
}

fn render_project_status(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &DashApp,
    state: &ProjectScreenState,
) {
    let spinner = super::SPINNER_FRAMES[app.spinner_tick % super::SPINNER_FRAMES.len()];

    let title_line = if let Some(project) = &state.project {
        Line::from(vec![
            Span::styled("project: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(project.name.as_str()),
            Span::raw("  •  "),
            Span::styled("workspace: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(project.workspace_name.as_deref().unwrap_or("personal")),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "project id: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(state.target.project_id.as_str()),
        ])
    };

    let detail_line = if let Some(error) = &state.error {
        Line::from(vec![
            Span::styled("error: ", error_style()),
            Span::raw(error),
        ])
    } else if state.loading {
        Line::from(vec![
            Span::styled(format!("{spinner} "), loading_style()),
            Span::styled(
                "Refreshing project overview...",
                Style::default().fg(RAILWAY_LAVENDER),
            ),
        ])
    } else if let Some(project) = &state.project {
        let accessible_count = project.accessible_environments().len();
        let selected_service = state
            .selected_service()
            .map(|service| service.name.as_str())
            .unwrap_or("none");
        Line::from(vec![
            Span::styled(
                "environment: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(project.selected_environment_name.as_str()),
            Span::raw("  •  "),
            Span::styled(
                format!("{} accessible envs", accessible_count),
                muted_style(),
            ),
            Span::raw("  •  "),
            Span::styled(
                format!("{} services", project.services.len()),
                muted_style(),
            ),
            Span::raw("  •  "),
            Span::styled("selected: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(selected_service),
        ])
    } else {
        Line::from("Waiting for project data...")
    };

    frame.render_widget(
        Paragraph::new(vec![title_line, Line::default(), detail_line])
            .block(panel_block("project"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_project_diagram(frame: &mut Frame<'_>, area: Rect, state: &ProjectScreenState) {
    let block = panel_block("services");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < SERVICE_CARD_MIN_WIDTH || inner.height < SERVICE_CARD_HEIGHT {
        frame.render_widget(
            Paragraph::new("Terminal too small for service cards. Resize to continue.")
                .style(accent_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let Some(project) = &state.project else {
        return;
    };

    if project.services.is_empty() {
        frame.render_widget(
            Paragraph::new("This project has no services yet.")
                .style(muted_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let columns = service_grid_columns_for_area(area);
    let card_width = service_card_width(inner.width, columns);
    let rows_per_page = service_rows_per_page(inner.height).max(1);
    let selected_row = state.selected_service / columns;
    let start_row = selected_row.saturating_sub(rows_per_page.saturating_sub(1));
    let start_index = start_row * columns;
    let end_index = (start_index + (rows_per_page * columns)).min(project.services.len());

    for (visible_index, service) in project.services[start_index..end_index].iter().enumerate() {
        let absolute_index = start_index + visible_index;
        let row = visible_index / columns;
        let col = visible_index % columns;
        let x = inner.x + (col as u16 * (card_width + SERVICE_CARD_GAP));
        let y = inner.y + (row as u16 * (SERVICE_CARD_HEIGHT + SERVICE_CARD_GAP));
        let rect = Rect {
            x,
            y,
            width: card_width,
            height: SERVICE_CARD_HEIGHT,
        };
        render_service_card(
            frame,
            rect,
            service,
            absolute_index == state.selected_service,
        );
    }
}

fn render_service_card(
    frame: &mut Frame<'_>,
    area: Rect,
    service: &DashboardService,
    selected: bool,
) {
    let border_style = if selected {
        selected_border_style()
    } else if service.active_in_environment {
        panel_border_style()
    } else {
        error_style()
    };
    let title_style = if selected {
        selected_title_style()
    } else if service.active_in_environment {
        hero_style()
    } else {
        Style::default()
            .fg(RAILWAY_ERROR)
            .add_modifier(Modifier::BOLD)
    };
    let status = service
        .latest_deployment
        .as_ref()
        .map(|deployment| deployment.status.as_str())
        .unwrap_or(if service.active_in_environment {
            "no latest deployment"
        } else {
            "no instance in env"
        });
    let replicas = service
        .num_replicas
        .map(|replicas| replicas.to_string())
        .unwrap_or_else(|| "-".to_string());

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(service.name.as_str(), title_style)),
            Line::default(),
            Line::from(vec![
                Span::styled("status: ", muted_style()),
                Span::raw(status),
            ]),
            Line::from(vec![
                Span::styled("replicas: ", muted_style()),
                Span::raw(replicas),
            ]),
            Line::from(vec![
                Span::styled("domains: ", muted_style()),
                Span::raw(service.domains.len().to_string()),
            ]),
        ])
        .block(panel_block(service.id.as_str()).border_style(border_style))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_project_summary(frame: &mut Frame<'_>, area: Rect, state: &ProjectScreenState) {
    let Some(project) = &state.project else {
        frame.render_widget(
            Paragraph::new("Project summary will appear once the overview loads.")
                .style(muted_style())
                .block(panel_block("summary"))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };

    let Some(service) = state.selected_service() else {
        frame.render_widget(
            Paragraph::new("No service selected.")
                .style(muted_style())
                .block(panel_block("summary"))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };

    let workspace_name = project.workspace_name.as_deref().unwrap_or("personal");
    let deployment_status = service
        .latest_deployment
        .as_ref()
        .map(|deployment| deployment.status.as_str())
        .unwrap_or("no deployment");
    let deployment_id = service
        .latest_deployment
        .as_ref()
        .map(|deployment| deployment.id.as_str())
        .unwrap_or("-");
    let domains = if service.domains.is_empty() {
        "none".to_string()
    } else {
        service.domains.join("\n")
    };
    let volumes = if service.volume_mounts.is_empty() {
        "none".to_string()
    } else {
        service.volume_mounts.join("\n")
    };
    let source = service
        .source_repo
        .as_deref()
        .or(service.source_image.as_deref())
        .unwrap_or("none");

    let lines = vec![
        Line::from(Span::styled(service.name.as_str(), hero_style())),
        Line::default(),
        Line::from(vec![
            Span::styled("project: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(project.name.as_str()),
        ]),
        Line::from(vec![
            Span::styled("workspace: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(workspace_name),
        ]),
        Line::from(vec![
            Span::styled(
                "environment: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(project.selected_environment_name.as_str()),
        ]),
        Line::default(),
        Line::from(vec![
            Span::styled("status: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(deployment_status),
        ]),
        Line::from(vec![
            Span::styled(
                "deployment: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(deployment_id),
        ]),
        Line::from(vec![
            Span::styled("replicas: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(
                service
                    .num_replicas
                    .map(|replicas| replicas.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ),
        ]),
        Line::from(vec![
            Span::styled("source: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(source),
        ]),
        Line::from(vec![
            Span::styled("cron: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(service.cron_schedule.as_deref().unwrap_or("none")),
        ]),
        Line::from(vec![
            Span::styled("command: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(service.start_command.as_deref().unwrap_or("none")),
        ]),
        Line::default(),
        Line::from(Span::styled("domains", panel_title_style())),
        Line::from(domains),
        Line::default(),
        Line::from(Span::styled("volumes", panel_title_style())),
        Line::from(volumes),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("selected service").border_style(selected_border_style()))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_environment_selector(frame: &mut Frame<'_>, area: Rect, state: &ProjectScreenState) {
    frame.render_widget(Clear, area);

    let Some(project) = &state.project else {
        return;
    };
    let Some(selector) = &state.environment_selector else {
        return;
    };

    let environments = project.accessible_environments();
    let mut lines = vec![Line::from(Span::styled(
        "Choose an environment",
        hero_style(),
    ))];
    lines.push(Line::default());

    for (index, environment) in environments.iter().enumerate() {
        let is_selected = index == selector.selected;
        let marker = if environment.id == project.selected_environment_id {
            "●"
        } else {
            "○"
        };
        let prefix = if is_selected { ">" } else { " " };
        let style = if is_selected {
            selected_title_style()
        } else {
            Style::default().fg(RAILWAY_LAVENDER)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("{prefix} {marker} "), style),
            Span::styled(
                environment.name.as_str(),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(environment.id.as_str(), muted_style()),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("environment selector").border_style(selected_border_style()))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_centered_message(
    frame: &mut Frame<'_>,
    area: Rect,
    message: &str,
    color: Color,
    title: &str,
) {
    frame.render_widget(
        Paragraph::new(message)
            .style(Style::default().fg(color))
            .block(panel_block(title))
            .wrap(Wrap { trim: true }),
        area,
    );
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

fn service_card_width(width: u16, columns: usize) -> u16 {
    let columns = columns.max(1) as u16;
    let total_gaps = SERVICE_CARD_GAP.saturating_mul(columns.saturating_sub(1));
    width.saturating_sub(total_gaps) / columns
}

fn service_rows_per_page(height: u16) -> usize {
    let stride = SERVICE_CARD_HEIGHT + SERVICE_CARD_GAP;
    ((height + SERVICE_CARD_GAP) / stride).max(1) as usize
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
