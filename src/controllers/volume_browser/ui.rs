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
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .split(area);

    render_header(app, frame, chunks[0]);
    render_entries(app, frame, chunks[1]);
    render_help_bar(app, frame, chunks[2]);
    render_status(app, frame, chunks[3]);

    match app.mode {
        BrowserMode::UploadInput => render_upload_popup(app, frame, area),
        BrowserMode::ConfirmOverwrite => render_confirm_popup(app, frame, area),
        BrowserMode::Help => render_help_popup(frame, area),
        BrowserMode::Browse => {}
    }
}

fn render_header(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(vec![
            Span::styled("  Volume ", Style::default().fg(LABEL_COLOR)),
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
            Span::styled("  Remote ", Style::default().fg(LABEL_COLOR)),
            Span::raw(app.current_path.display().to_string()),
            Span::styled("  Local ", Style::default().fg(LABEL_COLOR)),
            Span::raw(app.local_dir.display().to_string()),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
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
                let marker = if entry.is_dir { "[-]" } else { "---" };
                let name = if entry.is_dir {
                    format!("{marker} {}/", entry.name)
                } else {
                    format!("{marker} {}", entry.name)
                };
                ListItem::new(Line::from(name))
            })
            .collect()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::BOTTOM)
                .border_style(Style::default().fg(BORDER_COLOR)),
        )
        .highlight_style(SELECTED_STYLE);

    let mut state = ListState::default();
    if !app.entries.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_help_bar(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let help = match app.mode {
        BrowserMode::Browse => {
            "Up/Down move  Enter open  Left parent  d download  u upload  r refresh  ? help  q quit"
        }
        BrowserMode::UploadInput => "Type local path  Enter upload  Esc cancel",
        BrowserMode::ConfirmOverwrite => "Enter/y overwrite  n/Esc cancel",
        BrowserMode::Help => "Esc close help",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(LABEL_COLOR),
        ))),
        area,
    );
}

fn render_status(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let mut lines = Vec::new();
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
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn render_upload_popup(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let popup = centered_rect(72, 5, area);
    frame.render_widget(Clear, popup);
    let input = Paragraph::new(vec![
        Line::from("Local path to upload"),
        Line::from(Span::styled(
            app.upload_input.clone(),
            Style::default().fg(Color::White),
        )),
    ])
    .block(Block::default().borders(Borders::ALL).title(" Upload "));
    frame.render_widget(input, popup);
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
        Line::from("Enter or Right    Open directory"),
        Line::from("Left or Backspace Parent directory"),
        Line::from("d                 Download selected file or directory"),
        Line::from("u                 Upload local file or directory"),
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
