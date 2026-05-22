use chrono_humanize::HumanTime;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::{
    hero_style, muted_style, panel_block, panel_title_style, project_overview_sections,
    project_sections, selected_border_style,
};
use crate::controllers::dash_tui::service::ServiceScreenState;

pub(super) fn render_service_screen(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    frame.render_widget(Clear, area);

    let [status_area, main_area] = project_sections(area);
    let [details_area, resources_area] = project_overview_sections(main_area);

    render_service_status(frame, status_area, state);
    render_service_overview(frame, details_area, state);
    render_service_resources(frame, resources_area, state);
}

fn render_service_status(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    let workspace_name = state.detail.workspace_name.as_deref().unwrap_or("personal");

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("service: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(state.detail.service.name.as_str(), hero_style()),
                Span::raw("  •  "),
                Span::styled("id: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(state.detail.service.id.as_str()),
            ]),
            Line::default(),
            Line::from(vec![
                Span::styled("project: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(state.detail.project_name.as_str()),
                Span::raw("  •  "),
                Span::styled(
                    "environment: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(state.detail.environment_name.as_str()),
                Span::raw("  •  "),
                Span::styled("workspace: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(workspace_name),
            ]),
        ])
        .block(panel_block("service"))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_service_overview(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    let service = &state.detail.service;
    let latest_deployment_id = service
        .latest_deployment
        .as_ref()
        .map(|deployment| deployment.id.as_str())
        .unwrap_or("none");
    let latest_deployment_status = service
        .latest_deployment
        .as_ref()
        .map(|deployment| deployment.status.as_str())
        .unwrap_or("none");
    let latest_deployment_created = service
        .latest_deployment
        .as_ref()
        .map(|deployment| HumanTime::from(deployment.created_at).to_string())
        .unwrap_or_else(|| "none".to_string());
    let latest_deployment_stopped = service
        .latest_deployment
        .as_ref()
        .map(|deployment| if deployment.stopped { "yes" } else { "no" })
        .unwrap_or("unknown");
    let source_repo = service.source_repo.as_deref().unwrap_or("none");
    let source_image = service.source_image.as_deref().unwrap_or("none");
    let next_cron_run = service
        .next_cron_run_at
        .map(|next_run| HumanTime::from(next_run).to_string())
        .unwrap_or_else(|| "none".to_string());

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("overview", panel_title_style())),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    "active in env: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(if service.active_in_environment {
                    "yes"
                } else {
                    "no"
                }),
            ]),
            Line::from(vec![
                Span::styled("replicas: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(
                    service
                        .num_replicas
                        .map(|replicas| replicas.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                ),
            ]),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    "deployment id: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(latest_deployment_id),
            ]),
            Line::from(vec![
                Span::styled(
                    "deployment status: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(latest_deployment_status),
            ]),
            Line::from(vec![
                Span::styled(
                    "deployment created: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(latest_deployment_created),
            ]),
            Line::from(vec![
                Span::styled("stopped: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(latest_deployment_stopped),
            ]),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    "source repo: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(source_repo),
            ]),
            Line::from(vec![
                Span::styled(
                    "source image: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(source_image),
            ]),
            Line::from(vec![
                Span::styled(
                    "cron schedule: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(service.cron_schedule.as_deref().unwrap_or("none")),
            ]),
            Line::from(vec![
                Span::styled(
                    "next cron run: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(next_cron_run),
            ]),
            Line::from(vec![
                Span::styled(
                    "start command: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(service.start_command.as_deref().unwrap_or("none")),
            ]),
        ])
        .block(panel_block("details").border_style(selected_border_style()))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_service_resources(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    let service = &state.detail.service;
    let domains = if service.domains.is_empty() {
        "none".to_string()
    } else {
        service.domains.join("\n")
    };
    let volumes = if service.volume_mounts.is_empty() {
        "none".to_string()
    } else {
        service.volume_mounts.join("\n")
    };

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("domains", panel_title_style())),
            Line::from(domains),
            Line::default(),
            Line::from(Span::styled("volumes", panel_title_style())),
            Line::from(volumes),
            Line::default(),
            Line::from(Span::styled("up next", panel_title_style())),
            Line::from(Span::styled("d deployments", muted_style())),
            Line::from(Span::styled("m metrics", muted_style())),
            Line::from(Span::styled("l logs", muted_style())),
            Line::from(Span::styled("v/f volume browser", muted_style())),
            Line::from(Span::styled("R redeploy latest", muted_style())),
        ])
        .block(panel_block("resources"))
        .wrap(Wrap { trim: true }),
        area,
    );
}
