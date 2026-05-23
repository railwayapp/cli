use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::super::{DashApp, RAILWAY_ERROR, RAILWAY_LAVENDER, RAILWAY_VIOLET, SPINNER_FRAMES};
use super::{
    SERVICE_CARD_GAP, SERVICE_CARD_HEIGHT, SERVICE_CARD_MIN_WIDTH, accent_style, centered_rect,
    error_style, hero_style, loading_style, muted_style, panel_block, panel_border_style,
    project_overview_sections, render_centered_message, screen_sections, selected_border_style,
    selected_title_style, service_card_width, service_grid_columns, service_rows_per_page,
};
use crate::controllers::dash_tui::data::DashboardService;
use crate::controllers::dash_tui::project::ProjectScreenState;

pub(super) fn render_project_screen(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &DashApp,
    state: &ProjectScreenState,
) {
    frame.render_widget(Clear, area);

    let [status_area, main_area] = screen_sections(area);

    render_project_status(frame, status_area, app, state);

    if state.loading && state.project.is_none() {
        render_centered_message(
            frame,
            main_area,
            &format!(
                "{} Loading project overview...",
                SPINNER_FRAMES[app.spinner_tick % SPINNER_FRAMES.len()]
            ),
            RAILWAY_VIOLET,
            "project overview",
        );
        return;
    }

    if let Some(error) = &state.error
        && state.project.is_none()
    {
        render_centered_message(
            frame,
            main_area,
            &format!("Unable to open project.\n\n{error}\n\nPress r to retry."),
            RAILWAY_ERROR,
            "project overview",
        );
        return;
    }

    let [diagram_area, summary_area] = project_overview_sections(main_area);

    render_project_diagram(frame, diagram_area, state);
    render_project_summary(frame, summary_area, state);

    if state.environment_selector.is_some() {
        render_environment_selector(frame, centered_rect(main_area, 55, 60), state);
    }
}

fn render_project_status(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &DashApp,
    state: &ProjectScreenState,
) {
    let spinner = SPINNER_FRAMES[app.spinner_tick % SPINNER_FRAMES.len()];

    let title_line = if let Some(project) = &state.project {
        Line::from(vec![
            Span::styled("project: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(project.name.as_str()),
            Span::raw("  •  "),
            Span::styled("workspace: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(project.workspace_name.as_deref().unwrap_or("personal")),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "project id: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(state.target.project_id.as_str()),
        ])
    };

    let detail_line = if let Some(error) = &state.error {
        Line::from(vec![
            Span::styled("error: ", error_style()),
            Span::raw(error),
        ])
    } else if state.loading {
        Line::from(vec![
            Span::styled(format!("{spinner} "), loading_style()),
            Span::styled(
                "Refreshing project overview...",
                Style::default().fg(RAILWAY_LAVENDER),
            ),
        ])
    } else if let Some(project) = &state.project {
        let accessible_count = project.accessible_environments().len();
        let selected_service = state
            .selected_service()
            .map(|service| service.name.as_str())
            .unwrap_or("none");
        Line::from(vec![
            Span::styled(
                "environment: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(project.selected_environment_name.as_str()),
            Span::raw("  •  "),
            Span::styled(
                format!("{} accessible envs", accessible_count),
                muted_style(),
            ),
            Span::raw("  •  "),
            Span::styled(
                format!("{} services", project.services.len()),
                muted_style(),
            ),
            Span::raw("  •  "),
            Span::styled("selected: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(selected_service),
        ])
    } else {
        Line::from("Waiting for project data...")
    };

    frame.render_widget(
        Paragraph::new(vec![title_line, Line::default(), detail_line])
            .block(panel_block("project"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_project_diagram(frame: &mut Frame<'_>, area: Rect, state: &ProjectScreenState) {
    let block = panel_block("services");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < SERVICE_CARD_MIN_WIDTH || inner.height < SERVICE_CARD_HEIGHT {
        frame.render_widget(
            Paragraph::new("Terminal too small for service cards. Resize to continue.")
                .style(accent_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let Some(project) = &state.project else {
        return;
    };

    if project.services.is_empty() {
        frame.render_widget(
            Paragraph::new("This project has no services yet.")
                .style(muted_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let columns = service_grid_columns(inner.width);
    let card_width = service_card_width(inner.width, columns);
    let rows_per_page = service_rows_per_page(inner.height);
    let selected_row = state.selected_service / columns;
    let start_row = selected_row.saturating_sub(rows_per_page.saturating_sub(1));
    let start_index = start_row * columns;
    let end_index = (start_index + (rows_per_page * columns)).min(project.services.len());

    for (visible_index, service) in project.services[start_index..end_index].iter().enumerate() {
        let absolute_index = start_index + visible_index;
        let row = visible_index / columns;
        let col = visible_index % columns;
        let x = inner.x + (col as u16 * (card_width + SERVICE_CARD_GAP));
        let y = inner.y + (row as u16 * (SERVICE_CARD_HEIGHT + SERVICE_CARD_GAP));
        let rect = Rect {
            x,
            y,
            width: card_width,
            height: SERVICE_CARD_HEIGHT,
        };
        render_service_card(
            frame,
            rect,
            service,
            absolute_index == state.selected_service,
        );
    }
}

fn render_service_card(
    frame: &mut Frame<'_>,
    area: Rect,
    service: &DashboardService,
    selected: bool,
) {
    let border_style = if selected {
        selected_border_style()
    } else if service.active_in_environment {
        panel_border_style()
    } else {
        error_style()
    };
    let title_style = if selected {
        selected_title_style()
    } else if service.active_in_environment {
        hero_style()
    } else {
        Style::default()
            .fg(RAILWAY_ERROR)
            .add_modifier(Modifier::BOLD)
    };
    let status = service
        .latest_deployment
        .as_ref()
        .map(|deployment| deployment.status.as_str())
        .unwrap_or(if service.active_in_environment {
            "no latest deployment"
        } else {
            "no instance in env"
        });
    let replicas = service
        .num_replicas
        .map(|replicas| replicas.to_string())
        .unwrap_or_else(|| "-".to_string());

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(service.name.as_str(), title_style)),
            Line::default(),
            Line::from(vec![
                Span::styled("status: ", muted_style()),
                Span::raw(status),
            ]),
            Line::from(vec![
                Span::styled("replicas: ", muted_style()),
                Span::raw(replicas),
            ]),
            Line::from(vec![
                Span::styled("domains: ", muted_style()),
                Span::raw(service.domains.len().to_string()),
            ]),
        ])
        .block(panel_block(service.id.as_str()).border_style(border_style))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_project_summary(frame: &mut Frame<'_>, area: Rect, state: &ProjectScreenState) {
    if state.project.is_none() {
        frame.render_widget(
            Paragraph::new("Project summary will appear once the overview loads.")
                .style(muted_style())
                .block(panel_block("summary"))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let Some(service) = state.selected_service() else {
        frame.render_widget(
            Paragraph::new("No service selected.")
                .style(muted_style())
                .block(panel_block("summary"))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };

    let label_style = Style::default().add_modifier(Modifier::BOLD);
    let status = service
        .latest_deployment
        .as_ref()
        .map(|deployment| deployment.status.as_str())
        .unwrap_or(if service.active_in_environment {
            "no latest deployment"
        } else {
            "no instance in env"
        });
    let deployment = service
        .latest_deployment
        .as_ref()
        .map(|deployment| deployment.id.as_str())
        .unwrap_or("none");
    let replicas = service
        .num_replicas
        .map(|replicas| replicas.to_string())
        .unwrap_or_else(|| "-".to_string());
    let domains = match service.domains.len() {
        0 => "no domains".to_string(),
        1 => "1 domain".to_string(),
        count => format!("{count} domains"),
    };

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(service.name.as_str(), hero_style())),
            Line::default(),
            Line::from(vec![
                Span::styled("status: ", label_style),
                Span::raw(status),
            ]),
            Line::from(vec![
                Span::styled("deployment: ", label_style),
                Span::raw(deployment),
            ]),
            Line::from(vec![
                Span::styled("replicas: ", label_style),
                Span::raw(replicas),
            ]),
            Line::from(vec![
                Span::styled("domains: ", label_style),
                Span::raw(domains),
            ]),
            Line::default(),
            Line::from(Span::styled(
                "Press Enter for full service details.",
                muted_style(),
            )),
        ])
        .block(panel_block("selected service").border_style(selected_border_style()))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_environment_selector(frame: &mut Frame<'_>, area: Rect, state: &ProjectScreenState) {
    frame.render_widget(Clear, area);

    let Some(project) = &state.project else {
        return;
    };
    let Some(selector) = &state.environment_selector else {
        return;
    };

    let environments = project.accessible_environments();
    let mut lines = vec![Line::from(Span::styled(
        "Choose an environment",
        hero_style(),
    ))];
    lines.push(Line::default());

    for (index, environment) in environments.iter().enumerate() {
        let is_selected = index == selector.selected;
        let marker = if environment.id == project.selected_environment_id {
            "●"
        } else {
            "○"
        };
        let prefix = if is_selected { ">" } else { " " };
        let style = if is_selected {
            selected_title_style()
        } else {
            Style::default().fg(RAILWAY_LAVENDER)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("{prefix} {marker} "), style),
            Span::styled(
                environment.name.as_str(),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(environment.id.as_str(), muted_style()),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("environment selector").border_style(selected_border_style()))
            .wrap(Wrap { trim: true }),
        area,
    );
}
