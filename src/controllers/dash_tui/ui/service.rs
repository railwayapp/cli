use chrono_humanize::HumanTime;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use serde_json::to_string_pretty;

use super::{
    centered_rect, error_style, hero_style, loading_style, muted_style, panel_block,
    panel_title_style, screen_sections, selected_border_style, selected_title_style,
};
use crate::{
    commands::queries::deployments::DeploymentStatus,
    controllers::{
        dash_tui::service::{ServiceConfirmationState, ServiceFocus, ServiceScreenState},
        deployment::ServiceDeployment,
    },
};

pub(super) fn render_service_screen(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    frame.render_widget(Clear, area);

    let [status_area, main_area] = screen_sections(area);
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

    if state.deployment_dialog.is_some() {
        render_service_deployment_dialog(frame, centered_rect(main_area, 70, 62), state);
    }

    if state.confirmation.is_some() {
        render_service_confirmation(frame, centered_rect(main_area, 60, 42), state);
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
            Line::from(Span::styled("m open metrics dashboard", muted_style())),
            Line::from(Span::styled("L open filtered logs", muted_style())),
            Line::from(Span::styled("d focus deployments", muted_style())),
            Line::from(Span::styled(
                "Enter inspect selected deployment",
                muted_style(),
            )),
            Line::from(Span::styled("r restart current service", muted_style())),
            Line::from(Span::styled(
                "D redeploy selected deployment",
                muted_style(),
            )),
            Line::from(Span::styled(
                "R rollback to selected deployment",
                muted_style(),
            )),
        ])
        .block(panel_block("resources"))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_service_deployments(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    let is_focused = matches!(state.focus, ServiceFocus::Deployments);
    let block = if is_focused {
        panel_block("deployments").border_style(selected_border_style())
    } else {
        panel_block("deployments")
    };
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
        if is_focused {
            "deployments focused • Enter view details"
        } else {
            "press d to focus"
        },
        muted_style(),
    )])];
    lines.push(Line::default());

    let entries_per_page = ((inner.height.saturating_sub(2)) / 4).max(1) as usize;
    let selected = state
        .selected_deployment
        .min(state.deployments.len().saturating_sub(1));
    let start = selected.saturating_sub(entries_per_page.saturating_sub(1));
    let end = (start + entries_per_page).min(state.deployments.len());

    for (index, deployment) in state.deployments[start..end].iter().enumerate() {
        let absolute_index = start + index;
        let is_current = current_deployment_id == Some(deployment.id.as_str());
        let is_selected = absolute_index == selected;
        let status_style = deployment_style(deployment, is_current, is_selected, is_focused);
        let age = HumanTime::from(deployment.created_at).to_string();
        let prefix = if is_selected { ">" } else { " " };

        lines.push(Line::from(vec![
            Span::styled(
                format!("{prefix} "),
                if is_selected {
                    selected_border_style()
                } else {
                    muted_style()
                },
            ),
            Span::styled(status_glyph(deployment, is_current), status_style),
            Span::raw(" "),
            Span::styled(format_status(&deployment.status), status_style),
        ]));
        lines.push(Line::from(vec![Span::styled(
            deployment.id.as_str(),
            if is_selected {
                selected_title_style()
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            },
        )]));
        lines.push(Line::from(vec![Span::styled(age, muted_style())]));
        lines.push(Line::default());
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn deployment_style(
    deployment: &ServiceDeployment,
    is_current: bool,
    is_selected: bool,
    is_focused: bool,
) -> Style {
    let base = if is_current {
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
    };

    if is_selected && is_focused {
        base.add_modifier(Modifier::UNDERLINED)
    } else {
        base
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

fn render_service_deployment_dialog(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    frame.render_widget(Clear, area);

    let Some(deployment) = state.dialog_deployment() else {
        return;
    };

    let is_current = state
        .detail
        .service
        .latest_deployment
        .as_ref()
        .map(|latest| latest.id.as_str())
        == Some(deployment.id.as_str());
    let created_at = deployment
        .created_at
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string();
    let age = HumanTime::from(deployment.created_at).to_string();
    let meta = deployment
        .meta
        .as_ref()
        .map(|meta| to_string_pretty(meta).unwrap_or_else(|_| meta.to_string()))
        .unwrap_or_else(|| "none".to_string());
    let meta_preview = truncate_multiline(&meta, 12, 1000);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("deployment details", hero_style())),
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
                Span::raw(deployment.id.as_str()),
            ]),
            Line::from(vec![
                Span::styled("status: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format_status(&deployment.status)),
                Span::raw("  •  "),
                Span::styled("current: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(if is_current { "yes" } else { "no" }),
            ]),
            Line::from(vec![
                Span::styled("created: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(created_at),
            ]),
            Line::from(vec![
                Span::styled("age: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(age),
            ]),
            Line::default(),
            Line::from(Span::styled("meta", panel_title_style())),
            Line::from(meta_preview),
            Line::default(),
            Line::from(Span::styled(
                "D redeploy • R rollback • Esc close",
                muted_style(),
            )),
        ])
        .block(panel_block("deployment").border_style(selected_border_style()))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn truncate_multiline(text: &str, max_lines: usize, max_chars: usize) -> String {
    let mut truncated = String::new();
    let mut line_count = 0usize;

    for line in text.lines() {
        if line_count == max_lines || truncated.len() >= max_chars {
            break;
        }

        if !truncated.is_empty() {
            truncated.push('\n');
        }

        let remaining = max_chars.saturating_sub(truncated.len());
        if line.len() > remaining {
            truncated.push_str(&line[..remaining]);
            break;
        }

        truncated.push_str(line);
        line_count += 1;
    }

    if truncated.is_empty() {
        text.chars().take(max_chars).collect()
    } else if truncated.len() < text.len() {
        format!("{truncated}\n…")
    } else {
        truncated
    }
}

fn render_service_confirmation(frame: &mut Frame<'_>, area: Rect, state: &ServiceScreenState) {
    frame.render_widget(Clear, area);

    let (title, deployment_id) = match state.selected_confirmation() {
        Some(ServiceConfirmationState::Redeploy { deployment_id }) => {
            ("Confirm redeploy", deployment_id.as_str())
        }
        Some(ServiceConfirmationState::Restart { deployment_id }) => {
            ("Confirm restart", deployment_id.as_str())
        }
        Some(ServiceConfirmationState::Rollback { deployment_id }) => {
            ("Confirm rollback", deployment_id.as_str())
        }
        None => return,
    };

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(title, hero_style())),
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
        .block(panel_block("confirmation").border_style(selected_border_style()))
        .wrap(Wrap { trim: true }),
        area,
    );
}
