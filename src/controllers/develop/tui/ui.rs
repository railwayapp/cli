use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
};

use super::app::{Tab, TuiApp};
use super::log_store::LogEntry;

fn convert_color(c: colored::Color) -> Color {
    match c {
        colored::Color::Black => Color::Black,
        colored::Color::Red => Color::Red,
        colored::Color::Green => Color::Green,
        colored::Color::Yellow => Color::Yellow,
        colored::Color::Blue => Color::Blue,
        colored::Color::Magenta => Color::Magenta,
        colored::Color::Cyan => Color::Cyan,
        colored::Color::White => Color::White,
        colored::Color::BrightBlack => Color::DarkGray,
        colored::Color::BrightRed => Color::LightRed,
        colored::Color::BrightGreen => Color::LightGreen,
        colored::Color::BrightYellow => Color::LightYellow,
        colored::Color::BrightBlue => Color::LightBlue,
        colored::Color::BrightMagenta => Color::LightMagenta,
        colored::Color::BrightCyan => Color::LightCyan,
        colored::Color::BrightWhite => Color::White,
        colored::Color::TrueColor { r, g, b } => Color::Rgb(r, g, b),
    }
}

pub fn render(app: &TuiApp, frame: &mut Frame) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    render_tabs(app, frame, chunks[0]);
    render_logs(app, frame, chunks[1]);
    render_help_bar(app, frame, chunks[2]);
}

fn render_tabs(app: &TuiApp, frame: &mut Frame, area: ratatui::layout::Rect) {
    let mut titles: Vec<Line> = vec![Line::from("Local"), Line::from("Image")];

    for service in &app.services {
        let suffix = if service.is_docker { "" } else { " *" };
        titles.push(Line::from(format!("{}{}", service.name, suffix)));
    }

    let selected = app.tab_index();

    let tabs = Tabs::new(titles)
        .select(selected)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .divider("|");

    frame.render_widget(tabs, area);
}

fn render_logs(app: &TuiApp, frame: &mut Frame, area: ratatui::layout::Rect) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let total_logs = app.current_log_count();

    let start = if app.follow_mode {
        total_logs.saturating_sub(visible_height)
    } else {
        app.scroll_offset
    };

    let lines: Vec<Line> = match app.current_tab {
        Tab::Local => render_log_entries(
            &app.log_store.local_logs,
            &app.services,
            start,
            visible_height,
        ),
        Tab::Image => render_log_entries(
            &app.log_store.image_logs,
            &app.services,
            start,
            visible_height,
        ),
        Tab::Service(idx) => {
            if let Some(service) = app.services.get(idx) {
                if let Some(buffer) = app.log_store.services.get(idx) {
                    buffer
                        .lines
                        .iter()
                        .skip(start)
                        .take(visible_height)
                        .map(|log| {
                            let prefix = Span::styled(
                                format!("[{}] ", service.name),
                                Style::default().fg(convert_color(log.color)),
                            );
                            let content = Span::raw(&log.message);
                            Line::from(vec![prefix, content])
                        })
                        .collect()
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        }
    };

    let title = match app.current_tab {
        Tab::Local => "Local Services",
        Tab::Image => "Image Services",
        Tab::Service(idx) => app
            .services
            .get(idx)
            .map(|s| s.name.as_str())
            .unwrap_or("Service"),
    };

    let scroll_indicator = if total_logs > 0 {
        let pos = if app.follow_mode {
            total_logs
        } else {
            start + visible_height.min(total_logs)
        };
        format!(" [{}/{}]", pos, total_logs)
    } else {
        String::new()
    };

    let follow_indicator = if app.follow_mode { " [FOLLOW]" } else { "" };

    let block = Block::default()
        .title(format!("{}{}{}", title, scroll_indicator, follow_indicator))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_log_entries(
    logs: &std::collections::VecDeque<LogEntry>,
    services: &[super::app::ServiceInfo],
    start: usize,
    count: usize,
) -> Vec<Line<'static>> {
    logs.iter()
        .skip(start)
        .take(count)
        .map(|entry| {
            let service_name = services
                .get(entry.service_idx)
                .map(|s| s.name.as_str())
                .unwrap_or("unknown");

            let prefix = Span::styled(
                format!("[{}] ", service_name),
                Style::default().fg(convert_color(entry.line.color)),
            );
            let content = Span::raw(entry.line.message.clone());
            Line::from(vec![prefix, content])
        })
        .collect()
}

fn render_help_bar(_app: &TuiApp, frame: &mut Frame, area: ratatui::layout::Rect) {
    let help_text = vec![
        Span::styled("1-9", Style::default().fg(Color::Yellow)),
        Span::raw(" tab  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" cycle  "),
        Span::styled("j/k", Style::default().fg(Color::Yellow)),
        Span::raw(" scroll  "),
        Span::styled("g/G", Style::default().fg(Color::Yellow)),
        Span::raw(" top/bottom  "),
        Span::styled("f", Style::default().fg(Color::Yellow)),
        Span::raw(" follow  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ];

    let paragraph = Paragraph::new(Line::from(help_text));
    frame.render_widget(paragraph, area);
}
