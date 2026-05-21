use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap},
};

use super::app::{BrowserMode, ConfirmAction, ConfirmRequest, LocalEntry, VolumeBrowserApp};
use crate::commands::volume::sftp::VolumeFileEntry;

const LABEL_COLOR: Color = Color::DarkGray;
const BORDER_COLOR: Color = Color::DarkGray;
const DISABLED_COLOR: Color = Color::Indexed(244);
const SELECTED_STYLE: Style = Style::new()
    .fg(Color::White)
    .bg(Color::Indexed(238))
    .add_modifier(Modifier::BOLD);

pub fn render(app: &VolumeBrowserApp, frame: &mut Frame) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    if area.width < 76 || area.height < 20 {
        frame.render_widget(
            Paragraph::new("Terminal too small. Please resize (min 76x20).")
                .style(Style::default().fg(Color::Yellow)),
            area,
        );
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .split(area);

    render_header(app, frame, chunks[0]);
    render_body(app, frame, chunks[1]);
    render_status(app, frame, chunks[2]);
    render_help_bar(app, frame, chunks[3]);

    match app.mode {
        BrowserMode::Confirm => render_confirm(app, frame, area),
        BrowserMode::Help => render_help(frame, area),
        BrowserMode::Browse | BrowserMode::Upload => {
            if app.error.is_some() {
                render_error(app, frame, area);
            }
        }
    }
}

fn render_header(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled("  Browse ", Style::default().fg(LABEL_COLOR)),
        Span::styled(
            app.target_name.clone(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" at ", Style::default().fg(LABEL_COLOR)),
        Span::styled(
            app.mount_path.clone(),
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  remote ", Style::default().fg(LABEL_COLOR)),
        Span::styled(app.remote_dir.clone(), Style::default().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(vec![line, Line::from("")]), area);
}

fn render_status(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    if let Some(progress) = &app.transfer_progress {
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
        let ratio = if progress.total == 0 {
            0.0
        } else {
            progress.completed as f64 / progress.total as f64
        };

        render_progress_bar(
            frame,
            chunks[0],
            ratio,
            &format!("{}/{}", progress.completed, progress.total),
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Downloading ", Style::default().fg(LABEL_COLOR)),
                Span::raw(progress.current_path.clone()),
            ])),
            chunks[1],
        );
    } else if let Some(status) = &app.status {
        frame.render_widget(
            Paragraph::new(status.clone()).style(Style::default().fg(LABEL_COLOR)),
            area,
        );
    }
}

fn render_progress_bar(frame: &mut Frame, area: Rect, ratio: f64, label: &str) {
    let width = area.width as usize;
    if width == 0 {
        return;
    }

    let ratio = ratio.clamp(0.0, 1.0);
    let filled_width = (ratio * width as f64).round() as usize;
    let label_width = label.chars().count().min(width);
    let label_start = width.saturating_sub(label_width) / 2;
    let label_chars = label.chars().take(label_width).collect::<Vec<_>>();

    let mut spans = Vec::with_capacity(width);
    for index in 0..width {
        let label_index = index
            .checked_sub(label_start)
            .filter(|idx| *idx < label_width);
        let text = label_index
            .and_then(|idx| label_chars.get(idx).copied())
            .unwrap_or(' ')
            .to_string();

        let style = if index < filled_width {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(LABEL_COLOR).bg(Color::Indexed(238))
        };

        spans.push(Span::styled(text, style));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_body(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    if app.mode == BrowserMode::Upload {
        let columns = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(area);
        render_remote_table(app, frame, columns[0]);
        render_local_table(app, frame, columns[1]);
    } else {
        render_remote_table(app, frame, area);
    }
}

fn render_remote_table(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let rows = if app.remote_entries.is_empty() {
        vec![Row::new(vec![
            Cell::from(Span::styled("No files", Style::default().fg(LABEL_COLOR))),
            Cell::from(""),
        ])]
    } else {
        app.remote_entries
            .iter()
            .map(|entry| {
                let row = Row::new(vec![
                    Cell::from(remote_name(entry, app.is_busy())),
                    Cell::from(remote_meta(entry.kind, app.is_busy())),
                ]);

                if app.is_busy() {
                    row.style(disabled_tree_style())
                } else {
                    row
                }
            })
            .collect()
    };

    let mut state = TableState::default();
    if !app.is_busy() && !app.remote_entries.is_empty() {
        state.select(Some(app.remote_selected));
    }

    let title = " Remote files ";
    let table = Table::new(rows, [Constraint::Min(20), Constraint::Length(12)])
        .header(Row::new(vec!["Name", "Type"]).style(if app.is_busy() {
            disabled_tree_style()
        } else {
            Style::default()
                .fg(LABEL_COLOR)
                .add_modifier(Modifier::BOLD)
        }))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(BORDER_COLOR)),
        )
        .style(if app.is_busy() {
            disabled_tree_style()
        } else {
            Style::default()
        })
        .row_highlight_style(SELECTED_STYLE);

    frame.render_stateful_widget(table, area, &mut state);
}

fn render_local_table(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let rows = if app.local_entries.is_empty() {
        vec![Row::new(vec![Cell::from(Span::styled(
            "No files",
            Style::default().fg(LABEL_COLOR),
        ))])]
    } else {
        app.local_entries
            .iter()
            .map(|entry| {
                Row::new(vec![Cell::from(local_name(entry))]).style(local_row_style(entry))
            })
            .collect()
    };

    let mut state = TableState::default();
    if !app.local_entries.is_empty() {
        state.select(Some(app.local_selected));
    }

    let table = Table::new(rows, [Constraint::Percentage(100)])
        .header(
            Row::new(vec![app.local_cwd.display().to_string()]).style(
                Style::default()
                    .fg(LABEL_COLOR)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(
            Block::default()
                .title(" Upload from local cwd ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .row_highlight_style(SELECTED_STYLE);

    frame.render_stateful_widget(table, area, &mut state);
}

fn render_help_bar(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let help =
        match app.mode {
            BrowserMode::Browse => browse_help_items(app),
            BrowserMode::Upload => vec![
                ("Up/Down/Left", "move/parent"),
                ("Enter", "open/upload"),
                ("Esc", "remote files"),
                ("R", "refresh local"),
            ],
            BrowserMode::Confirm => {
                if app
                    .confirm
                    .as_ref()
                    .is_some_and(|confirm| confirm.action == ConfirmAction::Delete)
                {
                    vec![("Enter", "delete"), ("Esc", "cancel")]
                } else if app.confirm.as_ref().is_some_and(|confirm| {
                    confirm.is_dir && confirm.action == ConfirmAction::Download
                }) {
                    vec![("Enter", "overwrite"), ("A", "overwrite all")]
                } else {
                    vec![("Enter", "overwrite"), ("Esc", "cancel")]
                }
            }
            BrowserMode::Help => vec![("Esc", "close help")],
        };

    frame.render_widget(Paragraph::new(Line::from(help_spans(help))), area);
}

fn browse_help_items(app: &VolumeBrowserApp) -> Vec<(&'static str, &'static str)> {
    let mut items = vec![("Up/Down/Left", "move/parent"), ("Enter", "open folder")];

    if app.selected_remote().is_some() {
        items.push(("X", "delete"));
    }

    items.extend([("U", "upload"), ("D", "download")]);

    if app
        .selected_remote()
        .is_some_and(|entry| entry.kind != "directory")
    {
        items.push(("E", "edit"));
    }

    items.extend([("R", "refresh"), ("Q", "quit")]);
    items
}

fn render_confirm(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let Some(confirm) = &app.confirm else {
        return;
    };
    let popup = centered_rect(62, 10, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            confirm.title.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(confirm.message.clone()),
        Line::from(confirm_target_line(confirm)),
        Line::from(""),
        Line::from(help_spans(confirm_help_items(confirm))),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .padding(Padding::new(1, 1, 1, 1));
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        popup,
    );
}

fn render_error(app: &VolumeBrowserApp, frame: &mut Frame, area: Rect) {
    let Some(error) = &app.error else {
        return;
    };
    let popup = centered_rect(62, 7, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            "Action failed",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(error.clone()),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .padding(Padding::new(1, 1, 1, 1));
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        popup,
    );
}

fn help_spans(items: Vec<(&'static str, &'static str)>) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (idx, (key, label)) in items.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("  ", Style::default().fg(LABEL_COLOR)));
        }
        spans.push(Span::styled(
            key,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(LABEL_COLOR),
        ));
    }
    spans
}

fn confirm_help_items(confirm: &ConfirmRequest) -> Vec<(&'static str, &'static str)> {
    if confirm.action == ConfirmAction::Delete {
        return vec![("Enter", "delete"), ("Esc", "cancel")];
    }

    if confirm.is_dir && confirm.action == ConfirmAction::Download {
        vec![("Enter", "overwrite"), ("A", "overwrite all")]
    } else {
        vec![("Enter", "overwrite")]
    }
}

fn confirm_target_line(confirm: &ConfirmRequest) -> Line<'static> {
    let target = match confirm.action {
        ConfirmAction::Download => confirm
            .overwrite_path
            .as_ref()
            .unwrap_or(&confirm.local_path)
            .display()
            .to_string(),
        ConfirmAction::Upload => confirm.remote_path.clone(),
        ConfirmAction::Delete => confirm.remote_path.clone(),
    };

    Line::from(vec![
        Span::styled("Path ", Style::default().fg(LABEL_COLOR)),
        Span::styled(target, Style::default().fg(Color::White)),
    ])
}

fn render_help(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(66, 14, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            "Volume browser help",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Use arrow keys to move through remote files."),
        Line::from("Enter opens the selected remote folder."),
        Line::from("Left, Backspace, or H goes up to the parent remote folder."),
        Line::from("U opens a local cwd sidebar for file upload."),
        Line::from("D downloads the selected file or folder into the local cwd."),
        Line::from("X or Delete deletes the selected remote file or folder after confirmation."),
        Line::from("E opens the selected file in your editor and uploads it back."),
        Line::from("R refreshes the remote file list."),
        Line::from("J/K move down/up and L opens a folder."),
        Line::from(""),
        Line::from(Span::styled("Esc close", Style::default().fg(LABEL_COLOR))),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::new(1, 1, 1, 1));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn remote_name(entry: &VolumeFileEntry, refreshing: bool) -> Line<'static> {
    let suffix = if entry.kind == "directory" { "/" } else { "" };
    let label = format!("{}{}", entry.name, suffix);
    if refreshing {
        return Line::from(Span::raw(label));
    }

    let style = match entry.kind {
        "directory" => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
        "symlink" => Style::default().fg(Color::Cyan),
        _ => Style::default(),
    };
    Line::from(Span::styled(label, style))
}

fn local_name(entry: &LocalEntry) -> Line<'static> {
    let suffix = if entry.is_dir { "/" } else { "" };
    let style = if entry.is_dir {
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(Span::styled(format!("{}{}", entry.name, suffix), style))
}

fn remote_meta(value: impl Into<String>, refreshing: bool) -> Span<'static> {
    if refreshing {
        Span::raw(value.into())
    } else {
        Span::styled(value.into(), Style::default().fg(LABEL_COLOR))
    }
}

fn disabled_tree_style() -> Style {
    Style::default()
        .fg(DISABLED_COLOR)
        .add_modifier(Modifier::DIM)
}

fn local_row_style(entry: &LocalEntry) -> Style {
    if entry.is_dir {
        Style::default().fg(Color::Blue)
    } else {
        Style::default()
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(width)) / 2),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(vertical[1]);
    horizontal[1]
}
