mod logs;
mod project;
mod projects;
mod service;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use self::logs::render_logs_screen;
use self::project::render_project_screen;
use self::projects::render_projects_screen;
use self::service::render_service_screen;
use super::{
    DashApp, DashboardAuthMode, DashboardScreen, RAILWAY_ERROR, RAILWAY_LAVENDER, RAILWAY_MUTED,
    RAILWAY_PANEL, RAILWAY_PINK, RAILWAY_PURPLE, RAILWAY_VIOLET,
};

pub(super) const PROJECT_CARD_MIN_WIDTH: u16 = 30;
pub(super) const PROJECT_CARD_HEIGHT: u16 = 7;
pub(super) const PROJECT_CARD_GAP: u16 = 1;
pub(super) const SERVICE_CARD_MIN_WIDTH: u16 = 26;
pub(super) const SERVICE_CARD_HEIGHT: u16 = 7;
pub(super) const SERVICE_CARD_GAP: u16 = 1;

pub(super) fn render(frame: &mut Frame<'_>, app: &DashApp) {
    let [header, body, footer] = dashboard_sections(frame.area());

    render_header(frame, header, app);

    match &app.screen {
        DashboardScreen::Projects(state) => render_projects_screen(frame, body, app, state),
        DashboardScreen::Project(state) => render_project_screen(frame, body, app, state),
        DashboardScreen::Service(state) => render_service_screen(frame, body, state),
        DashboardScreen::Logs(state) => render_logs_screen(frame, body, state),
    }

    render_footer(frame, footer, app);
}

fn grid_metrics(area: Rect, min_width: u16, card_height: u16, gap: u16) -> (usize, usize, u16) {
    let columns = ((area.width + gap) / (min_width + gap)).max(1) as usize;
    let rows_per_page = ((area.height + gap) / (card_height + gap)).max(1) as usize;
    let card_width = area
        .width
        .saturating_sub(gap.saturating_mul(columns.saturating_sub(1) as u16))
        / columns as u16;
    (columns, rows_per_page, card_width)
}

pub(super) fn project_grid_metrics(area: Rect) -> (usize, usize, u16) {
    grid_metrics(
        area,
        PROJECT_CARD_MIN_WIDTH,
        PROJECT_CARD_HEIGHT,
        PROJECT_CARD_GAP,
    )
}

pub(super) fn service_grid_metrics(area: Rect) -> (usize, usize, u16) {
    grid_metrics(
        area,
        SERVICE_CARD_MIN_WIDTH,
        SERVICE_CARD_HEIGHT,
        SERVICE_CARD_GAP,
    )
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &DashApp) {
    let subtitle = match &app.screen {
        DashboardScreen::Projects(state) if state.filter_mode => "project cards • filtering",
        DashboardScreen::Projects(_) => "project cards",
        DashboardScreen::Project(state) if state.environment_selector.is_some() => {
            "project overview • environment selector"
        }
        DashboardScreen::Project(_) => "project overview",
        DashboardScreen::Service(_) => "service detail",
        DashboardScreen::Logs(_) => "logs",
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
                "Enter service • L logs • Esc back • e environments • arrows/ijkl move • r reload • q quit"
            }
            DashboardAuthMode::LinkedProject { .. } => {
                "Enter service • L logs • e environments • arrows/ijkl move • r reload • q quit"
            }
        },
        DashboardScreen::Service(_) => {
            "Tab switch panes • arrows/ik scroll • r restart service • Esc back • q quit"
        }
        DashboardScreen::Logs(_) => {
            "Esc back • p pause/resume • g/G top/bottom • arrows/ik scroll • q quit"
        }
    };

    frame.render_widget(
        Paragraph::new(text)
            .style(muted_style())
            .block(panel_block("controls"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

pub(super) fn dashboard_sections(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(3),
    ])
    .areas(area)
}

pub(super) fn screen_sections(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Length(4), Constraint::Min(8)]).areas(area)
}

pub(super) fn project_overview_sections(area: Rect) -> [Rect; 2] {
    Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)]).areas(area)
}

pub(super) fn panel_block<'a>(title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(panel_border_style())
        .title(Span::styled(title, panel_title_style()))
}

pub(super) fn panel_border_style() -> Style {
    Style::default().fg(RAILWAY_PANEL)
}

pub(super) fn panel_title_style() -> Style {
    Style::default()
        .fg(RAILWAY_PURPLE)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn hero_style() -> Style {
    Style::default()
        .fg(RAILWAY_LAVENDER)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn selected_border_style() -> Style {
    Style::default().fg(RAILWAY_PINK)
}

pub(super) fn selected_title_style() -> Style {
    Style::default()
        .fg(RAILWAY_PINK)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn loading_style() -> Style {
    Style::default().fg(RAILWAY_VIOLET)
}

pub(super) fn accent_style() -> Style {
    Style::default().fg(RAILWAY_PINK)
}

pub(super) fn muted_style() -> Style {
    Style::default().fg(RAILWAY_MUTED)
}

pub(super) fn error_style() -> Style {
    Style::default()
        .fg(RAILWAY_ERROR)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn render_centered_message(
    frame: &mut Frame<'_>,
    area: Rect,
    message: &str,
    color: ratatui::style::Color,
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

pub(super) fn pluralize(count: usize, singular: &str) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        format!("{singular}s")
    }
}

pub(super) fn centered_rect(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
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
