use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap},
};

use super::{RegionRow, ScaleTuiApp, ScaleTuiMode};

const LABEL_COLOR: Color = Color::DarkGray;

pub fn render(app: &ScaleTuiApp, frame: &mut Frame) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    if area.width < 72 || area.height < 18 {
        let warning = Paragraph::new("Terminal too small. Please resize (min 72x18).")
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(warning, area);
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .split(area);

    render_header(app, frame, chunks[0]);
    render_table(app, frame, chunks[1]);
    render_preview(app, frame, chunks[2]);
    render_help_bar(app, frame, chunks[3]);

    match app.mode {
        ScaleTuiMode::Confirm => render_confirm_popup(app, frame, area),
        ScaleTuiMode::Help => render_help_popup(frame, area),
        ScaleTuiMode::Browse | ScaleTuiMode::Search | ScaleTuiMode::Edit => {}
    }
}

fn render_header(app: &ScaleTuiApp, frame: &mut Frame, area: Rect) {
    let mut header = vec![
        Span::styled(
            format!("  Scale {}", app.service_name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  in  ", Style::default().fg(LABEL_COLOR)),
        Span::raw(app.environment_name.clone()),
    ];

    if app.mode == ScaleTuiMode::Search || !app.search.is_empty() {
        header.push(Span::styled("  search  ", Style::default().fg(LABEL_COLOR)));
        header.push(Span::styled(
            if app.search.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", app.search)
            },
            Style::default().fg(Color::Yellow),
        ));
    }

    frame.render_widget(
        Paragraph::new(vec![Line::from(header), Line::from("")]),
        area,
    );
}

fn render_table(app: &ScaleTuiApp, frame: &mut Frame, area: Rect) {
    let visible = app.visible_indices();
    if visible.is_empty() {
        let message = if app.search.is_empty() {
            "No regions available."
        } else {
            "No regions match the current search."
        };
        frame.render_widget(
            Paragraph::new(format!("  {message}")).style(Style::default().fg(LABEL_COLOR)),
            area,
        );
        return;
    }

    let rows = visible.iter().enumerate().map(|(visible_idx, idx)| {
        let row = &app.rows[*idx];
        Row::new(vec![
            Cell::from(region_label(row)),
            Cell::from(row.cli_name.clone()),
            replica_cell(app, visible_idx, row),
            Cell::from(change_label(row)),
        ])
        .style(row_style(row))
    });

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(52),
            Constraint::Length(18),
            Constraint::Length(12),
            Constraint::Min(18),
        ],
    )
    .header(
        Row::new(vec!["Region", "CLI name", "Replicas", "Change"]).style(
            Style::default()
                .fg(LABEL_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(Block::default().borders(Borders::TOP | Borders::BOTTOM))
    .row_highlight_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("  ");

    let mut state = TableState::default();
    state.select(Some(app.selected.min(visible.len().saturating_sub(1))));
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_preview(app: &ScaleTuiApp, frame: &mut Frame, area: Rect) {
    let mut lines = vec![
        Line::from(Span::styled(
            "Command preview",
            Style::default().fg(LABEL_COLOR),
        )),
        Line::from(app.command_preview()),
    ];

    if let Some(error) = &app.error {
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    } else if app.changes().is_empty() {
        lines.push(Line::from(Span::styled(
            "No scale changes selected.",
            Style::default().fg(LABEL_COLOR),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("{} region change(s) selected.", app.changes().len()),
            Style::default().fg(Color::Green),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().padding(Padding::new(1, 1, 0, 0)))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_help_bar(app: &ScaleTuiApp, frame: &mut Frame, area: Rect) {
    let help = match app.mode {
        ScaleTuiMode::Search => "Type search  Enter done  Esc clear/back  Up/Down move",
        ScaleTuiMode::Browse => {
            "Up/Down move  type edit  +/- adjust  0 remove  / search  a apply  q cancel  ? help"
        }
        ScaleTuiMode::Edit => "Type replicas  Enter save  Esc cancel  Backspace delete",
        ScaleTuiMode::Confirm => "Enter apply  e edit  q cancel",
        ScaleTuiMode::Help => "Esc close help",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(LABEL_COLOR),
        ))),
        area,
    );
}

fn render_confirm_popup(app: &ScaleTuiApp, frame: &mut Frame, area: Rect) {
    let popup = centered_rect(58, 12, area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(Span::styled(
            "Apply scale changes?",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for row in app.changed_rows().into_iter().take(5) {
        lines.push(Line::from(format!(
            "{}  {} -> {}",
            row.label, row.current, row.desired
        )));
    }

    let hidden = app.changed_rows().len().saturating_sub(5);
    if hidden > 0 {
        lines.push(Line::from(Span::styled(
            format!("and {hidden} more..."),
            Style::default().fg(LABEL_COLOR),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter apply  e edit  q cancel",
        Style::default().fg(LABEL_COLOR),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::new(1, 1, 1, 1));
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        popup,
    );
}

fn render_help_popup(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(62, 13, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            "Scale TUI help",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("+ / - adjusts the selected region by one replica."),
        Line::from("Type a number to edit the selected replica cell inline."),
        Line::from("Enter saves an inline edit."),
        Line::from("0 sets the selected region to zero replicas."),
        Line::from("/ searches by dashboard label, CLI name, or region id."),
        Line::from("a previews and applies the selected changes."),
        Line::from("q or Esc cancels without applying."),
        Line::from(""),
        Line::from(Span::styled("Esc close", Style::default().fg(LABEL_COLOR))),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::new(1, 1, 1, 1));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn region_label(row: &RegionRow) -> String {
    let mut label = row.label.clone();
    if row.dedicated {
        label.push_str(" [dedicated]");
    }
    if !row.available {
        label.push_str(" [unavailable]");
    }
    label
}

fn replica_cell(app: &ScaleTuiApp, visible_idx: usize, row: &RegionRow) -> Cell<'static> {
    let is_editing = app.mode == ScaleTuiMode::Edit && app.selected == visible_idx;
    if is_editing {
        return Cell::from(Line::from(vec![Span::styled(
            format!("[{}]", app.edit_input),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]));
    }

    let style = if row.changed() {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Cell::from(Line::from(Span::styled(row.desired.to_string(), style)))
}

fn change_label(row: &RegionRow) -> String {
    if !row.changed() {
        return String::new();
    }

    if row.desired == 0 && row.current > 0 {
        return format!("was {}, remove", row.current);
    }

    match row.change() {
        change if change > 0 => format!("was {}, +{}", row.current, change),
        change if change < 0 => format!("was {}, {}", row.current, change),
        _ => String::new(),
    }
}

fn row_style(row: &RegionRow) -> Style {
    if row.changed() {
        Style::default().fg(Color::Green)
    } else if !row.available {
        Style::default().fg(Color::Yellow)
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
