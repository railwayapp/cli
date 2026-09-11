//! Forms and progress stay centered in the pane that normally holds the terminal.
use super::*;
use crate::commands::cloud_agent::tui::bootstrap_setup::{Form, text_start};
use ratatui::layout::Margin;
use ratatui::widgets::Padding;

pub(super) fn render(app: &App, f: &mut Frame, rects: &mut PaneRects) {
    let host = if rects.session_outer.w > 0 {
        let p = rects.session_outer;
        Rect::new(p.x, p.y, p.w, p.h)
    } else {
        let area = page(f);
        Rect::new(
            area.x,
            area.y + 2,
            area.width,
            area.height.saturating_sub(3),
        )
    };
    f.render_widget(Clear, host);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.accent_dim))
        .title(Span::styled(
            " bootstraps ",
            Style::default().fg(app.theme.dim),
        ));
    let area = block.inner(host);
    f.render_widget(block, host);
    rects.session_outer = whole(host);
    rects.session = whole(area);
    rects.prompt = PaneBox::default();
    rects.bootstrap = PaneBox::default();
    if app.screen == Screen::BootstrapPick {
        render_picker(app, f, area, rects);
    } else if let Some(form) = &app.bootstrap_form {
        if form.running || form.finished {
            render_progress(app, form, f, area, rects);
        } else {
            render_form(app, form, f, area, rects);
        }
    }
}

fn card(app: &App, f: &mut Frame, area: Rect, title: &str, rects: &mut PaneRects) -> Rect {
    rects.bootstrap_card = whole(area);
    let block = dialog_block(app.theme)
        .title(format!(" {title} "))
        .padding(Padding::horizontal(2));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

fn text(app: &App, f: &mut Frame, area: Rect, value: &str, accent: bool) {
    f.render_widget(
        Paragraph::new(value.to_owned())
            .alignment(Alignment::Center)
            .style(Style::default().fg(if accent {
                app.theme.accent
            } else {
                app.theme.dim
            }))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn button(app: &App, f: &mut Frame, row: Rect, label: &str, focused: bool) -> Rect {
    let area = centered(
        (console::measure_text_width(label) as u16 + 2).min(row.width),
        1,
        row,
    );
    let style = Style::default()
        .fg(app.theme.on_accent)
        .bg(if focused {
            app.theme.accent
        } else {
            app.theme.accent_dim
        })
        .add_modifier(Modifier::BOLD);
    f.render_widget(
        Paragraph::new(format!(" {label} "))
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
    area
}

fn input(
    app: &App,
    f: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    placeholder: &str,
    cursor: Option<usize>,
) {
    let theme = app.theme;
    let focused = cursor.is_some();
    let block = dialog_block(theme)
        .title(format!(" {label} "))
        .padding(Padding::horizontal(1))
        .border_style(Style::default().fg(if focused {
            theme.accent
        } else {
            theme.accent_dim
        }));
    let inner = block.inner(area);
    let display = match cursor {
        Some(cursor) => {
            let cursor = cursor.min(value.len());
            let start = text_start(value, cursor, inner.width.saturating_sub(1) as usize);
            if value.is_empty() {
                format!("▏{placeholder}")
            } else {
                format!("{}▏{}", &value[start..cursor], &value[cursor..])
            }
        }
        None if value.is_empty() => placeholder.to_string(),
        None => value.to_string(),
    };
    f.render_widget(
        Paragraph::new(display)
            .style(Style::default().fg(if value.is_empty() {
                theme.dim
            } else {
                theme.fg
            }))
            .block(block),
        area,
    );
}

fn render_form(app: &App, form: &Form, f: &mut Frame, host: Rect, rects: &mut PaneRects) {
    let snapshot = form.snapshot.is_some();
    let width = 66.min(host.width.saturating_sub(2).max(1));
    let usable = width.saturating_sub(6).max(1);
    let description = if host.height < 21 {
        "Configure new Cloud Agents."
    } else if snapshot {
        "Save this VM's configuration as a bootstrap environment for new Cloud Agents."
    } else {
        "Create a bootstrap environment with your repo configuration included. New Cloud Agents will be created based off of this bootstrap."
    };
    let intro_h = wrapped_lines(description, usable as usize).min(4) as u16;
    let gap = u16::from(host.height >= if snapshot { 22 } else { 28 });
    let error_h = form
        .error
        .as_ref()
        .map_or(0, |e| wrapped_lines(e, usable as usize).min(6) as u16 + 1);
    let visible_fields = if snapshot {
        vec![0]
    } else if host.height < 17 + error_h {
        vec![form.field.min(2)]
    } else {
        vec![0, 1, 2]
    };
    let field_count = visible_fields.len() as u16;
    let height = 2 + intro_h + 1 + gap + field_count * (3 + gap) + 1 + gap + 1 + error_h;
    let area = centered(width, height, host);
    let inner = card(
        app,
        f,
        area,
        if snapshot {
            "Save VM as bootstrap"
        } else {
            "Create bootstrap"
        },
        rects,
    );
    let mut constraints = vec![
        Constraint::Length(intro_h),
        Constraint::Length(1),
        Constraint::Length(gap),
    ];
    for _ in 0..field_count {
        constraints.extend([Constraint::Length(3), Constraint::Length(gap)]);
    }
    constraints.extend([
        Constraint::Length(1),
        Constraint::Length(gap),
        Constraint::Length(1),
        Constraint::Length(error_h),
    ]);
    let rows = Layout::vertical(constraints).split(inner);
    text(app, f, rows[0], description, false);
    let target = if let Some(vm) = &form.snapshot {
        format!("{} · {}", form.target.label(), vm.agent_name)
    } else {
        form.target.label()
    };
    text(app, f, rows[1], &target, true);
    for (position, &index) in visible_fields.iter().enumerate() {
        let row = rows[3 + position * 2];
        match index {
            0 => input(
                app,
                f,
                row,
                "Name",
                &form.name,
                "e.g. development",
                (form.field == 0).then_some(form.name_cursor),
            ),
            1 => input(
                app,
                f,
                row,
                "Repository (optional)",
                &form.repo,
                "owner/repo or HTTPS URL",
                (form.field == 1).then_some(form.repo_cursor),
            ),
            _ => {
                let label =
                    super::super::app::harness_label(super::super::app::HARNESSES[form.harness]);
                input(
                    app,
                    f,
                    row,
                    "Coding agent",
                    &format!("‹  {label}  ›"),
                    "",
                    None,
                );
                if form.field == 2 {
                    f.render_widget(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(app.theme.accent))
                            .title(" Coding agent "),
                        row,
                    );
                }
            }
        }
        rects.bootstrap_fields[index] = whole(row);
    }
    let at = 3 + field_count as usize * 2;
    let default_text = if form.defaults_loading {
        "Checking the current default…".into()
    } else {
        format!(
            "{} {}",
            if form.make_default { "[✓]" } else { "[ ]" },
            if usable < 40 {
                "Make default"
            } else {
                "Make default for new Cloud Agents"
            }
        )
    };
    f.render_widget(
        Paragraph::new(default_text).style(Style::default().fg(
            if form.field == form.default_field() {
                app.theme.accent
            } else {
                app.theme.fg
            },
        )),
        rows[at],
    );
    rects.bootstrap_fields[form.default_field()] = whole(rows[at]);
    let button_area = button(
        app,
        f,
        rows[at + 2],
        "Create bootstrap",
        form.field == form.submit_field(),
    );
    rects.bootstrap_fields[form.submit_field()] = whole(button_area);
    if let Some(error) = &form.error {
        f.render_widget(
            Paragraph::new(error.clone())
                .style(Style::default().fg(app.theme.pending))
                .wrap(Wrap { trim: true }),
            rows[at + 3],
        );
    }
}

fn render_progress(app: &App, form: &Form, f: &mut Frame, host: Rect, rects: &mut PaneRects) {
    let area = centered(56, 10, host);
    let inner = card(
        app,
        f,
        area,
        if form.finished {
            "Bootstrap ready"
        } else {
            "Creating bootstrap"
        },
        rects,
    );
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(if form.finished { 2 } else { 3 }),
        Constraint::Length(1),
        Constraint::Length(if form.finished { 1 } else { 0 }),
    ])
    .split(inner);
    text(app, f, rows[0], &form.target.label(), false);
    text(
        app,
        f,
        rows[2],
        &if form.finished {
            "✓".into()
        } else {
            spinner_frame(app.loading.tick).to_string()
        },
        true,
    );
    text(
        app,
        f,
        rows[4],
        form.steps
            .last()
            .map(String::as_str)
            .unwrap_or("Preparing the bootstrap environment"),
        true,
    );
    if form.finished {
        let button_area = button(
            app,
            f,
            rows[6],
            if form.return_to_prompt {
                "Return to prompt"
            } else {
                "Done"
            },
            true,
        );
        rects.bootstrap_fields[0] = whole(button_area);
    }
}

fn render_picker(app: &App, f: &mut Frame, host: Rect, rects: &mut PaneRects) {
    let Some(picker) = &app.bootstrap_picker else {
        return;
    };
    let height =
        picker.entries.len().min(10) as u16 + 9 + if picker.error.is_some() { 2 } else { 0 };
    let area = centered(64, height, host.inner(Margin::new(1, 0)));
    let inner = card(app, f, area, "Select Bootstrap", rects);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(if picker.error.is_some() { 3 } else { 1 }),
    ])
    .split(inner);
    text(app, f, rows[0], &picker.target.label(), true);
    text(
        app,
        f,
        rows[1],
        if picker.for_launch {
            "Choose a bootstrap for this new VM."
        } else {
            "Choose the default for new Cloud Agents in this environment."
        },
        false,
    );
    if picker.loading || picker.saving {
        text(
            app,
            f,
            centered(rows[3].width, 2, rows[3]),
            if picker.saving {
                "Saving default…"
            } else {
                "Loading bootstraps…"
            },
            true,
        );
        return;
    }
    let visible = rows[3].height.max(1) as usize;
    let offset = picker.cursor.saturating_sub(visible - 1);
    rects.bootstrap_list = whole(rows[3]);
    rects.bootstrap_list_offset = offset;
    for (line, index) in (offset..picker.entries.len() + 2).take(visible).enumerate() {
        let none = index == picker.no_default_index();
        let is_default = if none {
            !picker.has_default
        } else {
            picker.entry_at(index).is_some_and(|b| b.is_default)
        };
        let label = if picker.for_launch && index == 0 {
            "Use project default".to_string()
        } else if !picker.for_launch && index == picker.create_index() {
            "+ Create New".to_string()
        } else if none {
            if picker.for_launch {
                "No bootstrap · clean VM".to_string()
            } else {
                "No Default".to_string()
            }
        } else {
            let Some(b) = picker.entry_at(index) else {
                continue;
            };
            if b.status == "READY" {
                b.name.clone()
            } else {
                format!("{} · saving", b.name)
            }
        };
        let selected = picker.cursor == index;
        let area = Rect::new(rows[3].x, rows[3].y + line as u16, rows[3].width, 1);
        let label = format!(
            "{} {}{}",
            if selected { "›" } else { " " },
            label,
            if is_default {
                if none {
                    "  ✓ selected"
                } else {
                    "  ✓ default"
                }
            } else {
                ""
            }
        );
        f.render_widget(
            Paragraph::new(label).style(if selected {
                Style::default()
                    .fg(app.theme.accent)
                    .bg(app.theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.fg)
            }),
            area,
        );
    }
    if let Some(error) = &picker.error {
        f.render_widget(
            Paragraph::new(error.clone())
                .style(Style::default().fg(app.theme.pending))
                .wrap(Wrap { trim: true }),
            rows[4],
        );
    }
}

pub(super) fn footer(app: &App, f: &mut Frame, area: Rect) {
    let hints = if app.screen == Screen::BootstrapPick {
        let for_launch = app.bootstrap_picker.as_ref().is_some_and(|p| p.for_launch);
        let mut hints = vec![
            ("↑↓", "select"),
            (
                "enter",
                if for_launch {
                    "select"
                } else if app
                    .bootstrap_picker
                    .as_ref()
                    .is_some_and(|p| p.cursor == p.create_index())
                {
                    "create new"
                } else {
                    "use default"
                },
            ),
            ("r", "refresh"),
            ("esc", "back"),
        ];
        if !for_launch {
            hints.insert(3, ("n", "new VM"));
        }
        hints
    } else if let Some(form) = &app.bootstrap_form {
        if form.running {
            vec![]
        } else if form.finished {
            vec![(
                "enter",
                if form.return_to_prompt {
                    "return to prompt"
                } else {
                    "done"
                },
            )]
        } else {
            vec![
                ("tab / ⇧tab", "field"),
                ("←→", "edit / choose"),
                ("enter", "select"),
                ("esc", "back"),
            ]
        }
    } else {
        vec![]
    };
    f.render_widget(
        Paragraph::new(Line::from(chord_spans(app.theme, &hints))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::super::tests::app_with_tree;
    use super::*;
    use crate::commands::cloud_agent::tui::app::{Effect, MouseAction};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn draw(app: &mut App, width: u16, height: u16, name: &str) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                app.panes = super::super::render_with_layout(app, f).0;
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut lines = vec![];
        let mut cells = vec![];
        for y in 0..height {
            let mut line = String::new();
            for x in 0..width {
                let c = &buffer[(x, y)];
                line.push_str(c.symbol());
                cells.push(serde_json::json!({"x":x,"y":y,"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg)}));
            }
            lines.push(line);
        }
        let text = lines.join("\n");
        if let Ok(dir) = std::env::var("RAILWAY_BOOTSTRAP_PREVIEW_DIR") {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                format!("{dir}/{name}-{width}x{height}.json"),
                serde_json::to_vec(
                    &serde_json::json!({"width":width,"height":height,"cells":cells}),
                )
                .unwrap(),
            )
            .unwrap();
            std::fs::write(format!("{dir}/{name}-{width}x{height}.txt"), &text).unwrap();
        }
        text
    }

    #[test]
    fn sidebar_drag_reveals_thread_text_and_expands_the_separator() {
        use crate::commands::cloud_agent::{client_sessions::Thread, tui::app::ConsoleSession};
        let mut app = app_with_tree();
        let title = "Refactor bootstrap selection and preserve repository configuration";
        let thread = Thread {
            id: "long-thread".into(),
            title: title.into(),
            directory: "/app".into(),
            created_at: None,
            updated_at: String::new(),
            state: "idle".into(),
        };
        app.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession::client_thread(
                "ca_1",
                "codex",
                Some(&thread),
            )]),
        );
        app.cursor = app
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session(..)))
            .unwrap();
        let sidebar = |out: &str, pane: PaneBox| {
            out.lines()
                .map(|line| {
                    line.chars()
                        .skip(pane.x as usize)
                        .take(pane.w as usize)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let out = draw(&mut app, 120, 40, "sidebar-default");
        let before = sidebar(&out, app.panes.tree);
        assert!(!before.contains(title));
        assert!(before.contains('…'));
        assert!(before.contains(&"─".repeat(28)));
        let edge = app.panes.sidebar_divider;
        app.on_mouse(MouseAction::Down, edge.x, edge.y);
        app.on_mouse(MouseAction::Drag, edge.x + 40, edge.y);
        let out = draw(&mut app, 120, 40, "sidebar-expanded");
        let after = sidebar(&out, app.panes.tree);
        assert!(after.contains(title), "{out}");
        assert!(after.contains(&"─".repeat(68)), "{out}");
        assert_eq!(
            app.on_mouse(MouseAction::Up, edge.x + 40, edge.y),
            Some(Effect::SaveSidebarWidth(72))
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|f| {
                app.panes = super::super::render_with_layout(&app, f).0;
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let y = app.panes.tree.y + app.panes.tree.h - 2;
        for x in app.panes.tree_outer.x..app.panes.tree_outer.x + app.panes.tree_outer.w {
            assert_eq!(buffer[(x, y)].bg, app.theme.sidebar);
        }
        assert_eq!(buffer[(app.panes.sidebar_divider.x, y)].symbol(), "│");
        assert_eq!(
            buffer[(app.panes.sidebar_divider.x + 1, y)].symbol(),
            " ",
            "only the sidebar owns the border between panes"
        );
    }

    #[test]
    fn bootstrap_form_centers_in_terminal_and_mouse_edits_and_submits() {
        for (w, h) in [(140, 45), (120, 40), (100, 30), (80, 24), (60, 24)] {
            let mut app = app_with_tree();
            app.start_bootstrap_setup();
            let out = draw(&mut app, w, h, "form");
            let pane = app.panes.session;
            let card = app.panes.bootstrap_card;
            assert!(card.x >= pane.x && card.y >= pane.y, "{out}");
            assert!(
                (card.x - pane.x).abs_diff(pane.x + pane.w - card.x - card.w) <= 1,
                "{out}"
            );
            assert!(
                (card.y - pane.y).abs_diff(pane.y + pane.h - card.y - card.h) <= 1,
                "{out}"
            );
            app.bootstrap_form.as_mut().unwrap().field = 4;
            draw(&mut app, w, h, "form-button-focused");
            let fields = app.panes.bootstrap_fields;
            for field in &fields[..3] {
                assert_eq!(field.x, fields[0].x, "fields align: {out}");
                assert_eq!(field.w, fields[0].w, "fields align: {out}");
                assert!(field.h >= 3, "fields need an interior row: {out}");
            }
            app.on_mouse(MouseAction::Down, fields[1].x + 2, fields[1].y + 1);
            app.on_paste("railwayapp/cli".into());
            app.on_mouse(MouseAction::Down, fields[0].x + 2, fields[0].y + 1);
            app.on_paste("development".into());
            app.on_mouse(MouseAction::Down, fields[3].x, fields[3].y);
            assert!(!app.bootstrap_form.as_ref().unwrap().make_default);
            let Some(Effect::CreateBootstrap(req)) =
                app.on_mouse(MouseAction::Down, fields[4].x + 1, fields[4].y)
            else {
                panic!("click submits: {out}")
            };
            assert_eq!(req.name, "development");
            assert_eq!(req.repo.as_deref(), Some("railwayapp/cli"));
            assert!(!req.make_default);
        }
    }

    #[test]
    fn bootstrap_progress_is_compact_and_only_shows_current_stage() {
        let mut app = app_with_tree();
        app.start_bootstrap_setup();
        let form = app.bootstrap_form.as_mut().unwrap();
        form.running = true;
        form.steps = vec![
            "Creating setup VM".into(),
            "Copying harness settings".into(),
            "Saving checkpoint".into(),
        ];
        let out = draw(&mut app, 120, 40, "progress");
        assert!(out.contains("Saving checkpoint"));
        assert!(!out.contains("Creating setup VM"));
        assert!(!out.contains("Copying harness settings"));
        assert_eq!(app.panes.bootstrap_card.h, 10);
        let form = app.bootstrap_form.as_mut().unwrap();
        form.running = false;
        form.finished = true;
        form.return_to_prompt = false;
        form.steps = vec!["'development' is your default for new Cloud Agents.".into()];
        let out = draw(&mut app, 120, 40, "ready");
        assert!(out.contains("Done"));
        assert!(!out.contains("return to prompt"));
        assert!(!out.contains("Return to prompt"));
        let button = app.panes.bootstrap_fields[0];
        app.on_mouse(MouseAction::Down, button.x + 1, button.y);
        assert_eq!(app.screen, Screen::Manage);
    }

    #[test]
    fn bootstrap_vm_name_form_and_vm_launch_progress_stay_in_the_pane() {
        let mut app = app_with_tree();
        app.cursor = app
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::Agent(..)))
            .unwrap();
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        app.bootstrap_form.as_mut().unwrap().defaults_loading = false;
        let out = draw(&mut app, 120, 40, "snapshot");
        assert!(out.contains("Save VM as bootstrap"));
        assert!(!out.contains("Repository (optional)"));
        assert!(!out.contains("return to prompt"));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.loading = super::super::super::app::Loading {
            active: true,
            target: "devtools/production".into(),
            harness: "claude".into(),
            prompt: Some("Fix the failing tests".into()),
            steps: vec![
                "Creating a cloud agent".into(),
                "Finalizing Configuration...".into(),
            ],
            tick: 0,
        };
        let out = draw(&mut app, 120, 40, "launch");
        assert!(out.contains("Finalizing Configuration..."));
        assert!(!out.contains("Creating a cloud agent"));
        assert!(out.contains("Fix the failing tests"));
    }

    fn ready_bootstrap(
        name: &str,
        default: bool,
    ) -> crate::controllers::agent_bootstrap::Bootstrap {
        crate::controllers::agent_bootstrap::Bootstrap {
            id: name.into(),
            name: name.into(),
            environment_id: "env_prod".into(),
            status: "READY".into(),
            failure_reason: None,
            updated_at: chrono::Utc::now(),
            is_default: default,
            source_agent_id: None,
            checkpoint_id: None,
        }
    }

    #[test]
    fn bootstrap_picker_hides_failed_captures_and_places_create_last() {
        let mut app = app_with_tree();
        app.tree[0].projects[0].envs[0].agents = Load::Loaded(vec![]);
        app.cursor = app
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::Project(..)))
            .unwrap();
        app.bootstrap_defaults.insert(
            "env_prod".into(),
            super::super::super::bootstrap_setup::DefaultState::Available,
        );
        assert!(draw(&mut app, 120, 40, "project-select").contains("Select Bootstrap"));
        app.start_bootstrap_picker();
        let mut failed = ready_bootstrap("failed-capture", false);
        failed.status = "DEGRADED".into();
        app.bootstrap_picker.as_mut().unwrap().loaded(Ok(vec![
            ready_bootstrap("alpha", false),
            failed,
            ready_bootstrap("beta", true),
        ]));
        assert_eq!(
            app.bootstrap_picker.as_ref().unwrap().cursor,
            app.bootstrap_picker.as_ref().unwrap().create_index()
        );
        let out = draw(&mut app, 120, 40, "picker-list");
        assert!(!out.contains("failed-capture"));
        let lines: Vec<_> = out.lines().collect();
        let create = lines
            .iter()
            .position(|l| l.contains("› + Create New"))
            .unwrap();
        assert!(lines[create - 3].contains("alpha"));
        assert!(lines[create - 2].contains("beta"));
        assert!(lines[create - 1].contains("No Default"));
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let out = draw(&mut app, 120, 40, "picker-selected");
        assert!(out.lines().nth(create - 3).unwrap().contains("› alpha"));
        let result = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            result,
            Some(Effect::SelectBootstrap {
                environment_id: "env_prod".into(),
                id: Some("alpha".into())
            })
        );
    }

    #[test]
    fn bootstrap_new_vm_picker_shows_codex_and_clickable_launch_choices() {
        for (width, height) in [(120, 40), (100, 30), (80, 24)] {
            let mut app = app_with_tree();
            app.cursor = app
                .rows()
                .iter()
                .position(|r| matches!(r.kind, RowKind::Agent(..)))
                .unwrap();
            app.bootstrap_defaults.insert(
                "env_prod".into(),
                super::super::super::bootstrap_setup::DefaultState::Ready("development".into()),
            );
            app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
            let out = draw(&mut app, width, height, "new-vm");
            assert!(out.contains("ChatGPT Codex"), "{out}");
            assert!(out.contains("Use bootstrap"), "{out}");
            assert!(out.contains("Select Bootstrap"), "{out}");
            let checkbox = app.panes.harness_use_bootstrap;
            assert_eq!(checkbox.x, app.panes.harness_list.x + 2);
            assert_eq!(app.panes.harness_bootstrap.x, app.panes.harness_list.x + 2);
            app.on_mouse(MouseAction::Down, checkbox.x, checkbox.y);
            assert!(!app.harness_use_bootstrap);
            let list = app.panes.harness_list;
            app.on_mouse(MouseAction::Down, list.x, list.y + 2);
            assert_eq!(
                super::super::super::app::HARNESSES[app.harness_pick.unwrap()],
                "codex"
            );
            let selector = app.panes.harness_bootstrap;
            assert!(matches!(
                app.on_mouse(MouseAction::Down, selector.x, selector.y),
                Some(Effect::LoadBootstraps { .. })
            ));
            app.bootstrap_picker
                .as_mut()
                .unwrap()
                .loaded(Ok(vec![ready_bootstrap("development", true)]));
            draw(&mut app, width, height, "launch-bootstrap");
            app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
            let out = draw(&mut app, width, height, "new-session");
            assert!(out.contains("New session"));
            assert!(!out.contains("Use bootstrap"));
            assert!(out.contains("ChatGPT Codex"));
        }
    }

    #[test]
    fn bootstrap_picker_scrolls_and_clicks_the_visible_entry() {
        let mut app = app_with_tree();
        app.start_bootstrap_picker();
        let picker = app.bootstrap_picker.as_mut().unwrap();
        picker.loaded(Ok((0..20)
            .map(|i| crate::controllers::agent_bootstrap::Bootstrap {
                id: format!("b{i:02}"),
                name: format!("bootstrap-{i:02}"),
                environment_id: "env_prod".into(),
                status: "READY".into(),
                failure_reason: None,
                source_agent_id: None,
                checkpoint_id: None,
                updated_at: chrono::Utc::now(),
                is_default: i == 0,
            })
            .collect()));
        picker.cursor = 21;
        let out = draw(&mut app, 120, 30, "picker-scrolled");
        assert!(out.contains("bootstrap-19"));
        let list = app.panes.bootstrap_list;
        let offset = app.panes.bootstrap_list_offset;
        let result = app.on_mouse(MouseAction::Down, list.x + 2, list.y);
        assert_eq!(
            result,
            Some(Effect::SelectBootstrap {
                environment_id: "env_prod".into(),
                id: Some(format!("b{offset:02}"))
            })
        );
    }

    #[test]
    fn bootstrap_project_actions_offer_create_and_no_default_without_changing_prompt_target() {
        let mut app = app_with_tree();
        app.tree[0].projects[0].envs[0].agents = Load::Loaded(vec![]);
        app.target.as_mut().unwrap().project_id = "unrelated-project".into();
        app.target.as_mut().unwrap().environment_id = "unrelated-env".into();
        let prompt_target = app.target.clone();
        app.cursor = app
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::Project(..)))
            .unwrap();
        let out = draw(&mut app, 140, 40, "project");
        assert!(out.contains("Create Bootstrap"));
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE)),
            Some(Effect::LoadBootstraps {
                environment_id: "env_prod".into()
            })
        );
        let picker = app.bootstrap_picker.as_mut().unwrap();
        picker.loaded(Ok(vec![]));
        let out = draw(&mut app, 120, 40, "picker");
        assert!(out.contains("+ Create New"));
        assert!(out.contains("No Default"));
        assert!(!out.contains("return to prompt"));
        assert_eq!(
            app.bootstrap_picker.as_ref().unwrap().cursor,
            app.bootstrap_picker.as_ref().unwrap().create_index()
        );
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let result = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            result,
            Some(Effect::SelectBootstrap {
                environment_id: "env_prod".into(),
                id: None
            })
        );
        assert_eq!(app.target, prompt_target);
    }
}
