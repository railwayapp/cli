use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::{
    hero_style, loading_style, muted_style, panel_block, screen_sections, selected_border_style,
};
use crate::controllers::dash_tui::logs::LogsScreenState;

pub(super) fn render_logs_screen(frame: &mut Frame<'_>, area: Rect, state: &LogsScreenState) {
    frame.render_widget(Clear, area);

    let [status_area, logs_area] = screen_sections(area);
    render_logs_status(frame, status_area, state);
    render_logs_output(frame, logs_area, state);
}

fn render_logs_status(frame: &mut Frame<'_>, area: Rect, state: &LogsScreenState) {
    let services = match state.service_count() {
        0 => "0 services".to_string(),
        1 => "1 service".to_string(),
        count => format!("{count} services"),
    };
    let scope = if state.is_service_scoped() {
        "service logs"
    } else {
        "environment logs"
    };
    let activity_line = if let Some(error) = &state.error {
        Line::from(vec![
            Span::styled("error: ", Style::default().fg(ratatui::style::Color::Red)),
            Span::raw(error.as_str()),
        ])
    } else if state.loading && state.lines.is_empty() {
        Line::from(vec![
            Span::styled("⠿ ", loading_style()),
            Span::styled(
                if state.is_service_scoped() {
                    "Loading service logs..."
                } else {
                    "Loading environment logs..."
                },
                hero_style(),
            ),
        ])
    } else if state.paused {
        Line::from(vec![
            Span::styled("paused", hero_style()),
            Span::raw(" • press p to resume following live logs"),
        ])
    } else {
        Line::from(vec![
            Span::styled("streaming", hero_style()),
            Span::raw(if state.is_service_scoped() {
                " • following latest log lines for the selected service"
            } else {
                " • following latest log lines across the environment"
            }),
        ])
    };

    let mut detail_spans = vec![
        Span::styled("project: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(state.project_name.as_str()),
        Span::raw("  •  "),
        Span::styled(
            "environment: ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(state.environment_name.as_str()),
    ];

    if let Some(service_name) = state.service_name.as_deref() {
        detail_spans.push(Span::raw("  •  "));
        detail_spans.push(Span::styled(
            "service: ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        detail_spans.push(Span::raw(service_name));
    }

    detail_spans.push(Span::raw("  •  "));
    detail_spans.push(Span::styled(
        "buffer: ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    detail_spans.push(Span::styled(state.lines.len().to_string(), muted_style()));

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("scope: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(scope, hero_style()),
                Span::raw("  •  "),
                Span::styled("services: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(services),
            ]),
            Line::from(detail_spans),
            activity_line,
        ])
        .block(panel_block("logs"))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_logs_output(frame: &mut Frame<'_>, area: Rect, state: &LogsScreenState) {
    let block = panel_block(if state.is_service_scoped() {
        "service deploy logs"
    } else {
        "environment deploy logs"
    })
    .border_style(selected_border_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.loading && state.lines.is_empty() {
        frame.render_widget(
            Paragraph::new(if state.is_service_scoped() {
                "Loading recent logs from the latest deployment for this service..."
            } else {
                "Loading recent logs from the latest deployments in this environment..."
            })
            .style(loading_style())
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    if state.lines.is_empty() {
        let message = if state.error.is_some() {
            "No logs loaded."
        } else if state.is_service_scoped() {
            "No deploy logs are available for this service yet."
        } else {
            "No deploy logs are available in this environment yet."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(muted_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let viewport_height = inner.height.max(1) as usize;
    let len = state.lines.len();
    let start = len.saturating_sub(viewport_height + state.scroll_offset_from_bottom);
    let visible = state
        .lines
        .iter()
        .skip(start)
        .take(viewport_height)
        .cloned();
    let lines = visible.map(Line::from).collect::<Vec<_>>();

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
