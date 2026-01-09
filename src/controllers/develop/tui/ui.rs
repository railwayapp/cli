use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Tabs},
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

pub fn render(app: &mut TuiApp, frame: &mut Frame) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let visible_height = chunks[1].height.saturating_sub(2) as usize;
    app.set_visible_height(visible_height);

    render_tabs(app, frame, chunks[0]);
    render_logs(app, frame, chunks[1]);
    render_info_pane(app, frame, chunks[2]);
    render_help_bar(app, frame, chunks[3]);
}

fn render_tabs(app: &TuiApp, frame: &mut Frame, area: ratatui::layout::Rect) {
    let mut titles: Vec<Line> = Vec::new();

    if app.show_local_tab() {
        titles.push(Line::from("Local"));
    }
    if app.show_image_tab() {
        titles.push(Line::from("Image"));
    }

    for service in &app.services {
        titles.push(Line::from(service.name.clone()));
    }

    let selected = app.tab_index();

    let tabs = Tabs::new(titles)
        .select(selected)
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
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
    // Clear prevents stale logs from previous tab rendering through
    frame.render_widget(Clear, area);
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

fn render_info_pane(app: &TuiApp, frame: &mut Frame, area: ratatui::layout::Rect) {
    let services_to_show: Vec<&super::app::ServiceInfo> = match app.current_tab {
        Tab::Local => app.services.iter().filter(|s| !s.is_docker).collect(),
        Tab::Image => app.services.iter().filter(|s| s.is_docker).collect(),
        Tab::Service(idx) => app.services.get(idx).into_iter().collect(),
    };

    let lines: Vec<Line> = services_to_show
        .iter()
        .map(|svc| {
            let mut spans = vec![Span::styled(
                format!("{}: ", svc.name),
                Style::default()
                    .fg(convert_color(svc.color))
                    .add_modifier(Modifier::BOLD),
            )];

            match (&svc.private_url, &svc.public_url) {
                (Some(priv_url), Some(pub_url)) => {
                    spans.push(Span::raw(priv_url.clone()));
                    spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
                    spans.push(Span::raw(pub_url.clone()));
                }
                (Some(url), None) | (None, Some(url)) => {
                    spans.push(Span::raw(url.clone()));
                }
                (None, None) => {
                    if let Some(cmd) = &svc.command {
                        spans.push(Span::styled(cmd.clone(), Style::default().fg(Color::Gray)));
                    } else if let Some(img) = &svc.image {
                        spans.push(Span::styled(img.clone(), Style::default().fg(Color::Gray)));
                    }
                }
            }

            spans.push(Span::styled(
                format!(" ({} vars)", svc.var_count),
                Style::default().fg(Color::DarkGray),
            ));

            Line::from(spans)
        })
        .collect();

    let title = match app.current_tab {
        Tab::Local => "Local Services",
        Tab::Image => "Image Services",
        Tab::Service(_) => "Service Info",
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
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
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::raw(" restart  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ];

    let paragraph = Paragraph::new(Line::from(help_text));
    frame.render_widget(paragraph, area);
}
