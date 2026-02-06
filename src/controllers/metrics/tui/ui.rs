use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Sparkline, Table},
};

use super::app::MetricsApp;

pub fn render(app: &mut MetricsApp, frame: &mut Frame) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Min(10),    // Main content
            Constraint::Length(3),  // Help bar
        ])
        .split(frame.area());

    render_header(app, frame, chunks[0]);
    render_main(app, frame, chunks[1]);
    render_help_bar(frame, chunks[2]);
}

fn render_header(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let refresh_text = if let Some(last) = app.last_refresh {
        format!("Updated: {}s ago", (chrono::Utc::now() - last).num_seconds())
    } else {
        "Refreshing...".to_string()
    };

    let title_text = format!(
        " Service Metrics │ {} │ {} ",
        app.time_range, refresh_text
    );

    // Build spans with error display if present
    let spans = if let Some(ref error) = app.last_error {
        let error_msg = if error.len() > 30 {
            format!(" ⚠ {}...", &error[..27])
        } else {
            format!(" ⚠ {}", error)
        };
        vec![
            Span::styled(title_text, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(error_msg, Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        ]
    } else {
        vec![
            Span::styled(title_text, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]
    };

    let paragraph = Paragraph::new(Line::from(spans))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

    frame.render_widget(paragraph, area);
}

fn render_main(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    if app.metrics.is_empty() {
        let paragraph = Paragraph::new("Loading metrics...")
            .style(Style::default().fg(Color::Yellow))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Metrics ")
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
        frame.render_widget(paragraph, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    render_services_table(app, frame, chunks[0]);
    render_details(app, frame, chunks[1]);
}

fn render_services_table(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let header = Row::new(vec!["Service", "CPU", "Memory", "Net RX", "Net TX"])
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .height(1);

    let rows: Vec<Row> = app
        .metrics
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let name = app.get_service_name(&m.service_id);
            let cpu = m
                .cpu_usage
                .last()
                .map(|s| format!("{:.1}%", s.value * 100.0))
                .unwrap_or_else(|| "-".to_string());
            let mem = m
                .memory_usage_gb
                .last()
                .map(|s| format!("{:.0} MB", s.value * 1024.0))
                .unwrap_or_else(|| "-".to_string());
            let rx = m
                .network_rx_gb
                .last()
                .map(|s| format!("{:.1} MB", s.value * 1024.0))
                .unwrap_or_else(|| "-".to_string());
            let tx = m
                .network_tx_gb
                .last()
                .map(|s| format!("{:.1} MB", s.value * 1024.0))
                .unwrap_or_else(|| "-".to_string());

            let style = if i == app.selected_service {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            Row::new(vec![name, cpu, mem, rx, tx]).style(style).height(1)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(30),
            Constraint::Percentage(15),
            Constraint::Percentage(20),
            Constraint::Percentage(17),
            Constraint::Percentage(18),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Services ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(table, area);
}

fn render_details(app: &MetricsApp, frame: &mut Frame, area: Rect) {
    let Some(metrics) = app.get_selected_metrics() else {
        let paragraph = Paragraph::new("Select a service to view details")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Details ")
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
        frame.render_widget(paragraph, area);
        return;
    };

    let service_name = app.get_service_name(&metrics.service_id);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(5),
        ])
        .split(area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", service_name))
        .border_style(Style::default().fg(Color::Cyan));
    frame.render_widget(block, area);

    render_sparkline_widget(
        frame,
        chunks[0],
        "CPU",
        &metrics.cpu_usage.iter().map(|s| s.value).collect::<Vec<_>>(),
        Color::Green,
        |v| format!("{:.1}%", v * 100.0),
    );

    render_sparkline_widget(
        frame,
        chunks[1],
        "Memory",
        &metrics
            .memory_usage_gb
            .iter()
            .map(|s| s.value)
            .collect::<Vec<_>>(),
        Color::Blue,
        |v| format!("{:.2} GB", v),
    );

    render_sparkline_widget(
        frame,
        chunks[2],
        "Net RX",
        &metrics
            .network_rx_gb
            .iter()
            .map(|s| s.value)
            .collect::<Vec<_>>(),
        Color::Yellow,
        |v| if v >= 1.0 {
            format!("{:.2} GB", v)
        } else {
            format!("{:.0} MB", v * 1024.0)
        },
    );

    render_sparkline_widget(
        frame,
        chunks[3],
        "Net TX",
        &metrics
            .network_tx_gb
            .iter()
            .map(|s| s.value)
            .collect::<Vec<_>>(),
        Color::Magenta,
        |v| if v >= 1.0 {
            format!("{:.2} GB", v)
        } else {
            format!("{:.0} MB", v * 1024.0)
        },
    );
}

fn render_sparkline_widget(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    data: &[f64],
    color: Color,
    formatter: impl Fn(f64) -> String,
) {
    if data.is_empty() {
        let paragraph = Paragraph::new(format!("{}: No data", label))
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(paragraph, area);
        return;
    }

    let max_val = data.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let scale = if max_val > 0.0 { 100.0 / max_val } else { 1.0 };
    let scaled: Vec<u64> = data.iter().map(|v| (v * scale) as u64).collect();

    let last_val = data.last().unwrap_or(&0.0);
    let title = format!("{}: {}", label, formatter(*last_val));

    let sparkline = Sparkline::default()
        .block(Block::default().title(title))
        .data(&scaled)
        .style(Style::default().fg(color));

    frame.render_widget(sparkline, area);
}

fn render_help_bar(frame: &mut Frame, area: Rect) {
    let help_text = vec![
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" Quit  "),
        Span::styled("↑/↓", Style::default().fg(Color::Yellow)),
        Span::raw(" Navigate  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" Next Service"),
    ];

    let paragraph = Paragraph::new(Line::from(help_text)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(paragraph, area);
}
