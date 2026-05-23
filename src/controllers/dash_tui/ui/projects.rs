use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::super::{DashApp, ProjectsScreenState, RAILWAY_LAVENDER, SPINNER_FRAMES};
use super::{
    PROJECT_CARD_GAP, PROJECT_CARD_HEIGHT, PROJECT_CARD_MIN_WIDTH, accent_style, error_style,
    hero_style, loading_style, muted_style, panel_block, panel_border_style, pluralize,
    project_card_width, project_grid_columns, project_rows_per_page, screen_sections,
    selected_border_style, selected_title_style,
};
use crate::controllers::dash_tui::data::ProjectCard;

pub(super) fn render_projects_screen(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &DashApp,
    state: &ProjectsScreenState,
) {
    frame.render_widget(Clear, area);

    let [status_area, grid_area] = screen_sections(area);

    render_projects_status(frame, status_area, app, state);
    render_projects_grid(frame, grid_area, state);
}

fn render_projects_status(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &DashApp,
    state: &ProjectsScreenState,
) {
    let selected = state
        .selected_card()
        .map(|card| card.name.as_str())
        .unwrap_or("none");
    let visible_count = state.visible_indices().len();
    let spinner = SPINNER_FRAMES[app.spinner_tick % SPINNER_FRAMES.len()];

    let filter_line = if state.filter_mode {
        Line::from(vec![
            Span::styled("filter: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("{}█", state.filter), selected_title_style()),
        ])
    } else if state.filter.is_empty() {
        Line::from(vec![
            Span::styled("filter: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("/ to search projects", muted_style()),
        ])
    } else {
        Line::from(vec![
            Span::styled("filter: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(state.filter.as_str(), selected_title_style()),
        ])
    };

    let status_line = if let Some(error) = &state.error {
        Line::from(vec![
            Span::styled("error: ", error_style()),
            Span::raw(error),
        ])
    } else if state.loading {
        let label = if state.cards.is_empty() {
            "Loading projects..."
        } else {
            "Refreshing projects..."
        };
        Line::from(vec![
            Span::styled(format!("{spinner} "), loading_style()),
            Span::styled(label, Style::default().fg(RAILWAY_LAVENDER)),
        ])
    } else {
        Line::from(vec![
            Span::styled("selected: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(selected, Style::default().fg(RAILWAY_LAVENDER)),
            Span::raw("  •  "),
            Span::styled(
                format!("{visible_count} visible / {} total", state.cards.len()),
                muted_style(),
            ),
        ])
    };

    frame.render_widget(
        Paragraph::new(vec![filter_line, Line::default(), status_line])
            .block(panel_block("projects"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_projects_grid(frame: &mut Frame<'_>, area: Rect, state: &ProjectsScreenState) {
    let block = panel_block("cards");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < PROJECT_CARD_MIN_WIDTH || inner.height < PROJECT_CARD_HEIGHT {
        frame.render_widget(
            Paragraph::new("Terminal too small for project cards. Resize to continue.")
                .style(accent_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let visible = state.visible_indices();
    if state.loading && state.cards.is_empty() {
        frame.render_widget(
            Paragraph::new("Loading projects...")
                .style(loading_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    if let Some(error) = &state.error
        && state.cards.is_empty()
    {
        frame.render_widget(
            Paragraph::new(format!(
                "Unable to load projects.\n\n{error}\n\nPress r to retry."
            ))
            .style(error_style())
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    if visible.is_empty() {
        let message = if state.filter.is_empty() {
            "No projects found for this account."
        } else {
            "No projects match the current filter."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(muted_style())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let columns = project_grid_columns(inner.width);
    let card_width = project_card_width(inner.width, columns);
    let rows_per_page = project_rows_per_page(inner.height);
    let selected_row = state.selected / columns;
    let start_row = selected_row.saturating_sub(rows_per_page.saturating_sub(1));
    let start_index = start_row * columns;
    let end_index = (start_index + (rows_per_page * columns)).min(visible.len());

    for (visible_index, card_index) in visible[start_index..end_index].iter().enumerate() {
        let absolute_visible_index = start_index + visible_index;
        let card = &state.cards[*card_index];
        let row = visible_index / columns;
        let col = visible_index % columns;
        let x = inner.x + (col as u16 * (card_width + PROJECT_CARD_GAP));
        let y = inner.y + (row as u16 * (PROJECT_CARD_HEIGHT + PROJECT_CARD_GAP));
        let rect = Rect {
            x,
            y,
            width: card_width,
            height: PROJECT_CARD_HEIGHT,
        };
        render_project_card(frame, rect, card, absolute_visible_index == state.selected);
    }
}

fn render_project_card(frame: &mut Frame<'_>, area: Rect, card: &ProjectCard, selected: bool) {
    let border_style = if selected {
        selected_border_style()
    } else {
        panel_border_style()
    };
    let title_style = if selected {
        selected_title_style()
    } else {
        hero_style()
    };

    let workspace_name = card.workspace_name.as_deref().unwrap_or("personal");
    let service_label = pluralize(card.service_count, "service");
    let environment_label = pluralize(card.environment_count, "environment");

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(card.name.as_str(), title_style)),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    format!("{} ", card.service_count),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(service_label),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("{} ", card.environment_count),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(environment_label),
            ]),
            Line::from(vec![
                Span::styled("workspace: ", muted_style()),
                Span::styled(workspace_name, Style::default().fg(RAILWAY_LAVENDER)),
            ]),
        ])
        .block(panel_block(card.id.as_str()).border_style(border_style))
        .wrap(Wrap { trim: true }),
        area,
    );
}
