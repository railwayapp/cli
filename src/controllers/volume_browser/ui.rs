use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use super::app::{BrowserMode, PendingTransfer, VolumeBrowserApp};

const LABEL_COLOR: Color = Color::DarkGray;
const BORDER_COLOR: Color = Color::DarkGray;
const ACCENT_COLOR: Color = Color::Cyan;
const SELECTED_STYLE: Style = Style::new()
    .fg(Color::White)
    .bg(Color::Indexed(238))
    .add_modifier(Modifier::BOLD);

pub fn render(app: &VolumeBrowserApp, frame: &mut Frame) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    if area.width < 72 || area.height < 18 {
        let warning = Paragraph::new("Terminal too small. Please resize (min 72x18).")
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(warning, area);
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(6),
        Constraint::Length(3),
    ])
    .split(area);

    render_header(app, frame, chunks[0]);
    if app.mode == BrowserMode::UploadPicker {
        let panes = Layout::horizontal([Constraint::Min(42), Constraint::Length(42)])
            .spacing(1)
            .split(chunks[1]);
        render_entries(app, frame, panes[0]);
        render_local_entries(app, frame, panes[1]);
    } else {
        render_entries(app, frame, chunks[1]);
    }
    render_footer(app, frame, chunks[2]);

    match app.mode {
        BrowserMode::UploadPicker => {}
        BrowserMode::ConfirmOverwrite => render_confirm_popup(app, frame, area),
        BrowserMode::Help => render_help_popup(frame, area),
        BrowserMode::Browse => {}
    }
}

fn render_header(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(BORDER_COLOR));
    let lines = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled("Volume ", Style::default().fg(LABEL_COLOR)),
            Span::styled(
                app.volume_name.clone(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" on ", Style::default().fg(LABEL_COLOR)),
            Span::styled(
                app.service_name.clone(),
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled("Remote ", Style::default().fg(LABEL_COLOR)),
            Span::styled(
                app.current_path.display().to_string(),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled("Local  ", Style::default().fg(LABEL_COLOR)),
            Span::styled(
                app.local_dir.display().to_string(),
                Style::default().fg(Color::White),
            ),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn render_entries(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let items = if app.entries.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  Directory is empty.",
            Style::default().fg(LABEL_COLOR),
        )))]
    } else {
        app.entries
            .iter()
            .map(|entry| {
                let marker = entry.kind.marker();
                let style = if entry.kind.is_dir() {
                    Style::default().fg(Color::Blue)
                } else {
                    Style::default().fg(Color::White)
                };
                let suffix = if entry.kind.is_dir() { "/" } else { "" };
                ListItem::new(Line::from(vec![
                    Span::styled(marker, Style::default().fg(LABEL_COLOR)),
                    Span::raw(" "),
                    Span::styled(format!("{}{suffix}", entry.name), style),
                ]))
            })
            .collect()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Files ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(BORDER_COLOR)),
        )
        .highlight_style(SELECTED_STYLE);

    let mut state = ListState::default();
    if !app.entries.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_local_entries(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let title = format!(" Upload from {} ", app.local_current_path.display());
    let items = if app.local_entries.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  Directory is empty.",
            Style::default().fg(LABEL_COLOR),
        )))]
    } else {
        app.local_entries
            .iter()
            .map(|entry| {
                let marker = if entry.is_dir { "[d]" } else { "[f]" };
                let suffix = if entry.is_dir { "/" } else { "" };
                let style = if entry.is_dir {
                    Style::default().fg(Color::Blue)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(marker, Style::default().fg(LABEL_COLOR)),
                    Span::raw(" "),
                    Span::styled(format!("{}{suffix}", entry.name), style),
                ]))
            })
            .collect()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT_COLOR)),
        )
        .highlight_style(SELECTED_STYLE);

    let mut state = ListState::default();
    if !app.local_entries.is_empty() {
        state.select(Some(app.local_selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_footer(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let mut lines = Vec::new();
    lines.push(help_line(app.mode));
    if let Some(error) = &app.error {
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    } else if let Some(status) = &app.status {
        lines.push(Line::from(Span::styled(
            status.clone(),
            Style::default().fg(Color::Green),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(BORDER_COLOR)),
            )
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn help_line(mode: BrowserMode) -> Line<'static> {
    let items: &[(&str, &str)] = match mode {
        BrowserMode::Browse => &[
            ("j/k", "move"),
            ("l/Enter", "open"),
            ("h/Left", "parent"),
            ("d", "download"),
            ("e", "edit"),
            ("u", "upload"),
            ("r", "refresh"),
            ("?", "help"),
            ("q", "quit"),
        ],
        BrowserMode::UploadPicker => &[
            ("j/k", "move"),
            ("l/Right", "open dir"),
            ("h/Left", "parent"),
            ("Enter", "upload"),
            ("r", "refresh"),
            ("Esc", "cancel"),
        ],
        BrowserMode::ConfirmOverwrite => &[("Enter/y", "overwrite"), ("n/Esc", "cancel")],
        BrowserMode::Help => &[("Esc", "close help")],
    };

    let mut spans = vec![Span::raw(" ")];
    for (idx, (key, label)) in items.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            *key,
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!(" {label}")));
    }
    Line::from(spans)
}

fn render_confirm_popup(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let popup = centered_rect(72, 6, area);
    frame.render_widget(Clear, popup);
    let target = match &app.pending_transfer {
        Some(PendingTransfer::Download { local, .. }) => local.display().to_string(),
        Some(PendingTransfer::Upload { remote, .. }) => remote.display().to_string(),
        None => "target".to_string(),
    };
    let content = Paragraph::new(vec![
        Line::from("Destination already exists."),
        Line::from(target),
        Line::from("Overwrite?"),
    ])
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::ALL).title(" Confirm "));
    frame.render_widget(content, popup);
}

fn render_help_popup(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(76, 11, area);
    frame.render_widget(Clear, popup);
    let help = Paragraph::new(vec![
        Line::from("Browse Railway volume files over SSH/SCP."),
        Line::from(""),
        Line::from("Up/Down or k/j    Move selection"),
        Line::from("Enter, Right, l   Open directory"),
        Line::from("Left, Backspace, h Parent directory"),
        Line::from("d                 Download selected file or directory"),
        Line::from("e                 Edit selected file and sync it back"),
        Line::from("u                 Open local upload picker"),
        Line::from("Enter             Upload selected local entry from picker"),
        Line::from("r                 Refresh"),
        Line::from("q or Esc          Quit"),
    ])
    .block(Block::default().borders(Borders::ALL).title(" Help "));
    frame.render_widget(help, popup);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(4));
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}
