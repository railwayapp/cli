use chrono_humanize::HumanTime;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::{
    centered_rect, error_style, hero_style, loading_style, muted_style, panel_block,
    panel_title_style, project_sections, selected_border_style,
};
use crate::{
    commands::queries::deployments::DeploymentStatus,
    controllers::{dash_tui::service::ServiceScreenState, deployment::ServiceDeployment},
};

pub(super) fn render_service_screen(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    frame.render_widget(Clear, area);

    let [status_area, main_area] = project_sections(area);
    let [content_area, sidebar_area] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
            .areas(main_area);
    let [details_area, resources_area] =
        Layout::vertical([Constraint::Percentage(68), Constraint::Percentage(32)])
            .areas(content_area);

    render_service_status(frame, status_area, state);
    render_service_overview(frame, details_area, state);
    render_service_resources(frame, resources_area, state);
    render_service_deployments(frame, sidebar_area, state);

    if state.redeploy_confirmation.is_some() {
        render_redeploy_confirmation(frame, centered_rect(main_area, 60, 42), state);
    }
}

fn render_service_status(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    let workspace_name = state.detail.workspace_name.as_deref().unwrap_or("personal");

    let state_line = if let Some(toast) = &state.toast {
        let label_style = if toast.is_error {
            error_style()
        } else {
            hero_style()
        };
        Line::from(vec![
            Span::styled(
                if toast.is_error { "error: " } else { "info: " },
                label_style,
            ),
            Span::raw(toast.message.as_str()),
        ])
    } else if let Some(error) = &state.error {
        Line::from(vec![
            Span::styled("error: ", error_style()),
            Span::raw(error.as_str()),
        ])
    } else if state.loading {
        Line::from(vec![
            Span::styled("⠿ ", loading_style()),
            Span::styled("Refreshing service detail...", hero_style()),
        ])
    } else {
        let latest = state
            .detail
            .service
            .latest_deployment
            .as_ref()
            .map(|deployment| deployment.id.as_str())
            .unwrap_or("none");
        Line::from(vec![
            Span::styled(
                "latest deployment: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(latest),
            Span::raw("  •  "),
            Span::styled(
                format!("{} deployments loaded", state.deployments.len()),
                muted_style(),
            ),
        ])
    };

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("service: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(state.detail.service.name.as_str(), hero_style()),
                Span::raw("  •  "),
                Span::styled("id: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(state.detail.service.id.as_str()),
            ]),
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
            state_line,
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
            Line::from(Span::styled("actions", panel_title_style())),
            Line::from(Span::styled("r refresh service", muted_style())),
            Line::from(Span::styled("R redeploy latest", muted_style())),
            Line::from(Span::styled("m metrics", muted_style())),
            Line::from(Span::styled("l logs", muted_style())),
        ])
        .block(panel_block("resources"))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_service_deployments(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    let block = panel_block("deployments");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.loading && state.deployments.is_empty() {
        frame.render_widget(
            Paragraph::new("Loading deployments...")
                .style(loading_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    if let Some(error) = &state.error
        && state.deployments.is_empty()
    {
        frame.render_widget(
            Paragraph::new(format!("Unable to load deployments.\n\n{error}"))
                .style(error_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    if state.deployments.is_empty() {
        frame.render_widget(
            Paragraph::new("No deployments found for this service.")
                .style(muted_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let current_deployment_id = state
        .detail
        .service
        .latest_deployment
        .as_ref()
        .map(|deployment| deployment.id.as_str());

    let mut lines = vec![Line::from(vec![Span::styled(
        "latest shown first",
        muted_style(),
    )])];
    lines.push(Line::default());

    for deployment in &state.deployments {
        let is_current = current_deployment_id == Some(deployment.id.as_str());
        let status_style = deployment_style(deployment, is_current);
        let age = HumanTime::from(deployment.created_at).to_string();

        lines.push(Line::from(vec![
            Span::styled(status_glyph(deployment, is_current), status_style),
            Span::raw(" "),
            Span::styled(format_status(&deployment.status), status_style),
        ]));
        lines.push(Line::from(vec![Span::styled(
            deployment.id.as_str(),
            Style::default().add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(vec![Span::styled(age, muted_style())]));
        lines.push(Line::default());
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn deployment_style(deployment: &ServiceDeployment, is_current: bool) -> Style {
    if is_current {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        match deployment.status {
            DeploymentStatus::FAILED | DeploymentStatus::CRASHED => error_style(),
            DeploymentStatus::REMOVED | DeploymentStatus::REMOVING | DeploymentStatus::SKIPPED => {
                muted_style()
            }
            DeploymentStatus::SUCCESS => hero_style(),
            _ => loading_style(),
        }
    }
}

fn status_glyph(deployment: &ServiceDeployment, is_current: bool) -> &'static str {
    if is_current {
        "●"
    } else {
        match deployment.status {
            DeploymentStatus::FAILED | DeploymentStatus::CRASHED => "✕",
            DeploymentStatus::REMOVED | DeploymentStatus::REMOVING | DeploymentStatus::SKIPPED => {
                "○"
            }
            DeploymentStatus::SUCCESS => "•",
            _ => "◌",
        }
    }
}

fn format_status(status: &DeploymentStatus) -> String {
    format!("{status:?}")
}

fn render_redeploy_confirmation(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    frame.render_widget(Clear, area);

    let deployment_id = state
        .redeploy_confirmation
        .as_ref()
        .map(|confirmation| confirmation.deployment_id.as_str())
        .unwrap_or("unknown");

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("Confirm redeploy", hero_style())),
            Line::default(),
            Line::from(vec![
                Span::styled("service: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(state.detail.service.name.as_str()),
            ]),
            Line::from(vec![
                Span::styled(
                    "environment: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(state.detail.environment_name.as_str()),
            ]),
            Line::from(vec![
                Span::styled(
                    "deployment: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(deployment_id),
            ]),
            Line::default(),
            Line::from(Span::styled("Enter confirm • Esc cancel", muted_style())),
        ])
        .block(panel_block("redeploy").border_style(selected_border_style()))
        .wrap(Wrap { trim: true }),
        area,
    );
}
