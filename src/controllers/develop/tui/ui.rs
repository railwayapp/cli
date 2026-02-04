use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
};

use super::app::{Selection, Tab, TuiApp};
use super::log_store::LogRef;

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
    let info_height = if app.show_info { 3 } else { 0 };

    let chunks = Layout::vertical([
        Constraint::Length(1),           // Tab bar
        Constraint::Min(1),              // Log area
        Constraint::Length(info_height), // Info pane
        Constraint::Length(1),           // Help bar
    ])
    .split(frame.area());

    // Store log area bounds for mouse handling
    app.set_log_area(chunks[1].y, chunks[1].height);

    render_tabs(app, frame, chunks[0]);
    render_logs(app, frame, chunks[1]);
    if app.show_info {
        render_info_pane(app, frame, chunks[2]);
    }
    render_help_bar(app, frame, chunks[3]);
}

fn render_tabs(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let mut titles: Vec<Line> = Vec::new();

    if app.show_local_tab() {
        titles.push(Line::from("Local"));
    }
    for service in &app.services {
        if !service.is_docker {
            titles.push(Line::from(service.name.clone()));
        }
    }
    if app.show_image_tab() {
        titles.push(Line::from("Image"));
    }
    for service in &app.services {
        if service.is_docker {
            titles.push(Line::from(service.name.clone()));
        }
    }

    let selected = app.tab_index();

    let follow_indicator = if app.follow_mode {
        Span::styled(" [FOLLOW]", Style::default().fg(Color::Green))
    } else {
        Span::styled(" [PAUSED]", Style::default().fg(Color::Yellow))
    };

    let tabs = Tabs::new(titles)
        .select(selected)
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("|");

    let tab_chunks = Layout::horizontal([Constraint::Min(10), Constraint::Length(10)]).split(area);

    frame.render_widget(tabs, tab_chunks[0]);
    frame.render_widget(Paragraph::new(Line::from(follow_indicator)), tab_chunks[1]);
}

fn render_logs(app: &mut TuiApp, frame: &mut Frame, area: Rect) {
    let visible_height = area.height as usize;
    app.set_visible_height(visible_height);

    // Get total count without collecting all logs
    let total = app.current_log_count();
    let start_idx = if app.follow_mode {
        total.saturating_sub(visible_height)
    } else {
        app.scroll_offset.min(total.saturating_sub(visible_height))
    };

    // Only collect visible logs
    let logs: Vec<LogRef> = match app.current_tab {
        Tab::Local => app
            .log_store
            .local_logs
            .iter()
            .skip(start_idx)
            .take(visible_height)
            .map(LogRef::Entry)
            .collect(),
        Tab::Image => app
            .log_store
            .image_logs
            .iter()
            .skip(start_idx)
            .take(visible_height)
            .map(LogRef::Entry)
            .collect(),
        Tab::Service(idx) => app
            .log_store
            .services
            .get(idx)
            .map(|buf| {
                buf.lines
                    .iter()
                    .skip(start_idx)
                    .take(visible_height)
                    .map(|line| LogRef::Service(idx, line))
                    .collect()
            })
            .unwrap_or_default(),
    };

    let mut lines: Vec<Line> = Vec::with_capacity(visible_height);

    for (vis_row, log_ref) in logs.iter().enumerate() {
        let (service_idx, service_name, message, log_color) = log_ref.parts();
        let service_color = app
            .services
            .get(service_idx)
            .map(|s| convert_color(s.color))
            .unwrap_or_else(|| convert_color(log_color));

        let line = render_log_line(
            service_name,
            service_color,
            message,
            vis_row,
            &app.selection,
        );
        lines.push(line);
    }

    // Fill remaining space
    while lines.len() < visible_height {
        lines.push(Line::from(""));
    }

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default());

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_log_line<'a>(
    service_name: &str,
    service_color: Color,
    message: &str,
    vis_row: usize,
    selection: &Option<Selection>,
) -> Line<'a> {
    let prefix = format!("[{service_name}] ");
    let prefix_len = prefix.chars().count();

    let Some(sel) = selection else {
        return Line::from(vec![
            Span::styled(prefix, Style::default().fg(service_color)),
            Span::raw(message.to_string()),
        ]);
    };

    let ((start_row, start_col), (end_row, end_col)) = sel.normalized();

    // Row not in selection at all
    if vis_row < start_row || vis_row > end_row {
        return Line::from(vec![
            Span::styled(prefix, Style::default().fg(service_color)),
            Span::raw(message.to_string()),
        ]);
    }

    let full_line = format!("{prefix}{message}");
    let chars: Vec<char> = full_line.chars().collect();
    let line_len = chars.len();

    // Calculate selection bounds for this row
    let sel_start = if vis_row == start_row { start_col } else { 0 };
    let sel_end = if vis_row == end_row {
        (end_col + 1).min(line_len)
    } else {
        line_len
    };

    // Build spans: [before selection] [selection] [after selection]
    let mut spans = Vec::new();

    if sel_start > 0 {
        let text: String = chars[..sel_start.min(line_len)].iter().collect();
        let style = if sel_start <= prefix_len {
            Style::default().fg(service_color)
        } else {
            Style::default()
        };
        // Split if spans both prefix and message
        if sel_start > prefix_len {
            let prefix_text: String = chars[..prefix_len].iter().collect();
            let msg_text: String = chars[prefix_len..sel_start].iter().collect();
            spans.push(Span::styled(
                prefix_text,
                Style::default().fg(service_color),
            ));
            spans.push(Span::raw(msg_text));
        } else {
            spans.push(Span::styled(text, style));
        }
    }

    if sel_start < line_len && sel_end > sel_start {
        let text: String = chars[sel_start..sel_end.min(line_len)].iter().collect();
        spans.push(Span::styled(
            text,
            Style::default().bg(Color::White).fg(Color::Black),
        ));
    }

    if sel_end < line_len {
        let text: String = chars[sel_end..].iter().collect();
        let style = if sel_end < prefix_len {
            Style::default().fg(service_color)
        } else {
            Style::default()
        };
        spans.push(Span::styled(text, style));
    }

    Line::from(spans)
}

fn render_info_pane(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let services_to_show: Vec<&super::app::ServiceInfo> = match app.current_tab {
        Tab::Local => app.services.iter().filter(|s| !s.is_docker).collect(),
        Tab::Image => app.services.iter().filter(|s| s.is_docker).collect(),
        Tab::Service(idx) => app.services.get(idx).into_iter().collect(),
    };

    // Limit to 2 services to fit within the fixed 3-line info pane height
    let lines: Vec<Line> = services_to_show
        .iter()
        .take(2)
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

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_help_bar(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let mut help_text = vec![
        Span::styled("1-9", Style::default().fg(Color::Yellow)),
        Span::raw(" tab  "),
        Span::styled("j/k", Style::default().fg(Color::Yellow)),
        Span::raw(" scroll  "),
        Span::styled("drag", Style::default().fg(Color::Yellow)),
        Span::raw(" copy  "),
        Span::styled("i", Style::default().fg(Color::Yellow)),
        Span::raw(if app.show_info { " hide" } else { " info" }),
        Span::raw("  "),
        Span::styled("f", Style::default().fg(Color::Yellow)),
        Span::raw(" follow  "),
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::raw(" restart  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ];

    // Show copy feedback
    if let Some(instant) = app.copied_feedback {
        if instant.elapsed().as_secs() < 2 {
            help_text.push(Span::styled(
                "  [Copied!]",
                Style::default().fg(Color::Green),
            ));
        }
    } else if let Some(instant) = app.copy_failed {
        if instant.elapsed().as_secs() < 2 {
            help_text.push(Span::styled(
                "  [Copy failed]",
                Style::default().fg(Color::Red),
            ));
        }
    }

    let paragraph = Paragraph::new(Line::from(help_text));
    frame.render_widget(paragraph, area);
}
