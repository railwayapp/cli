//! Rendering for the `railway ca` TUI. Pure draw code — every decision it
//! needs has already been made in [`super::app`].

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};

use super::app::{
    App, KEY_HELP, Load, LoadSessions, ManageFocus, MenuFocus, PaneBox, PaneRects, Row, RowKind,
    Screen,
};
use super::theme::Theme;

/// Drawn only when the terminal is wide and tall enough for it; below that the
/// screen still has to be usable, so a one-line wordmark stands in.
///
/// Full blocks and spaces only. The obvious figlet for this (ANSI Shadow) draws
/// its depth with `╗╔═║╚╝`, and those are box-drawing glyphs a monospace font is
/// free to render at a different weight or offset from `█` — which it does, and
/// the wordmark comes out looking sheared. Every font renders U+2588 as a full
/// cell, so a block-only mark is the same shape everywhere.
const BANNER: &str = r#"██████   █████  ██████ ██      ██   ██  █████  ██    ██
██   ██ ██   ██   ██   ██      ██   ██ ██   ██  ██  ██
██████  ███████   ██   ██      ██ █ ██ ███████   ████
██  ██  ██   ██   ██   ██      ███████ ██   ██    ██
██   ██ ██   ██ ██████ ███████ ██   ██ ██   ██    ██"#;

const BANNER_W: u16 = 55;
const BANNER_H: u16 = 5;

/// The column a menu card's name occupies, so the descriptions line up.
const LABEL_W: usize = 20;

/// The marker column plus the spaces either side of a card's name.
const CARD_GUTTER: usize = 3;

/// Width of the tree column in Manage, borders included.
const TREE_W: u16 = 32;

/// What a dialog spends on chrome: its two border cells. The breathing room
/// lives *outside* the boxes — see [`page`] — not between a border and its
/// text.
const DIALOG_CHROME_X: u16 = 2;
const DIALOG_CHROME_Y: u16 = 2;

/// Columns of space between the terminal's edge and the UI.
const PAGE_MARGIN_X: u16 = 2;
/// Rows of the same. Half the columns, because a terminal cell is about twice
/// as tall as it is wide — the same gap on screen on every side.
const PAGE_MARGIN_Y: u16 = 1;

/// What the page margin leaves of a `width` × `height` terminal. Skipped when
/// the terminal is too small to spend cells on air — a cramped layout beats a
/// truncated one.
///
/// The single source of that answer: [`page`] shapes what is drawn with it,
/// and [`session_pane_size`] shapes the PTY with it. They must agree — an
/// emulator wrapping wider than its pane puts the last columns of every row
/// somewhere the screen never shows, which shears anything long enough to
/// wrap (an OAuth URL loses four characters at every fold).
fn page_size(width: u16, height: u16) -> (u16, u16) {
    if width < 40 || height < 12 {
        (width, height)
    } else {
        (width - PAGE_MARGIN_X * 2, height - PAGE_MARGIN_Y * 2)
    }
}

/// The page: the frame minus a slim outer margin, so boxes never press
/// against the terminal's edges. Every screen and floating card lays out
/// against this.
fn page(f: &Frame) -> Rect {
    let area = f.area();
    let (width, height) = page_size(area.width, area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The chrome every floating card shares.
///
/// Opaque fill, because these sit *on top of* the screen rather than in it:
/// with only [`Clear`] underneath, the terminal's own background shows through
/// and the card reads as a hole punched in the page instead of a dialog above
/// it.
fn dialog_block(theme: &Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .style(Style::default().bg(theme.surface).fg(theme.fg))
}

/// One inverse-styled key badge, e.g. ` ^t `. The building block of
/// [`chord_spans`], and also used solo wherever a shortcut sits beside the
/// thing it acts on instead of in the footer's own chord list.
fn chord_badge(theme: &Theme, chord: &str) -> Span<'static> {
    Span::styled(
        format!(" {chord} "),
        Style::default()
            .fg(theme.on_accent)
            .bg(theme.accent_dim)
            .add_modifier(Modifier::BOLD),
    )
}

/// Footer chords: an inverse badge for the key, dim text for what it does.
/// Shared so the menu and the manage screen read as the same product.
fn chord_spans(theme: &Theme, chords: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(chords.len() * 2);
    for (chord, what) in chords {
        spans.push(chord_badge(theme, chord));
        spans.push(Span::styled(
            format!(" {what}   "),
            Style::default().fg(theme.dim),
        ));
    }
    spans
}

/// The wordmark as equal-width lines.
///
/// Each line is padded here rather than in the literal above: ratatui centres
/// every line independently, so a row that lost its trailing spaces would sit
/// half a character off from the rest — and trailing whitespace inside a source
/// literal is exactly the thing an editor or a formatter silently trims.
fn banner_lines(theme: &Theme) -> Vec<Line<'static>> {
    BANNER
        .lines()
        .map(|l| {
            let pad = (BANNER_W as usize).saturating_sub(l.chars().count());
            Line::from(Span::styled(
                format!("{l}{}", " ".repeat(pad)),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect()
}

/// Render; report where the panes ended up so the mouse can hit-test them, and
/// lift out any pending selection's text.
///
/// The text has to come from here because this is the only place the finished
/// frame exists: the session pane's contents are an emulator's screen composed
/// into a buffer, and what the user dragged over is that composition.
pub fn render_with_layout(app: &App, f: &mut Frame) -> (PaneRects, Option<String>) {
    let mut rects = PaneRects::default();
    render_inner(app, f, &mut rects);
    let text = app.pending_copy.and_then(|selection| {
        let bounds = match selection.pane {
            ManageFocus::Tree => rects.tree,
            ManageFocus::Session => rects.session,
        };
        let buffer = f.buffer_mut();
        let lines: Vec<String> = selection
            .spans(bounds)
            .into_iter()
            .map(|(y, x0, x1)| {
                let line: String = (x0..=x1).map(|x| buffer[(x, y)].symbol()).collect();
                line.trim_end().to_string()
            })
            .collect();
        let text = lines.join("\n");
        (!text.trim().is_empty()).then_some(text)
    });
    (rects, text)
}

fn render_inner(app: &App, f: &mut Frame, rects: &mut PaneRects) {
    f.render_widget(Clear, f.area());
    render_screen(app, f, rects);
    render_ssh_gate(app, f);
    render_toast(app, f);
}

/// The register-your-SSH-key question, centered over whatever raised it. Up
/// only while [`App::ssh_gate`] holds a connect (or setup's offer); the next
/// key answers it — see the gate block at the top of [`App::on_key`].
fn render_ssh_gate(app: &App, f: &mut Frame) {
    let Some(gate) = app.ssh_gate.as_ref() else {
        return;
    };
    let theme = app.theme;
    let area = page(f);
    // Name and fingerprint on their own lines: together they outrun the card
    // and wrap mid-fingerprint, which reads as garbage. Apart, both fit.
    // Everything reads in the foreground — this card is the only thing asking
    // for attention while it is up, so nothing on it is background noise.
    let body = Style::default().fg(theme.fg);
    let lines = vec![
        Line::from(Span::styled(
            format!("  {}", gate.offer.name),
            body.add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(format!("  {}", gate.offer.fingerprint), body)),
        Line::from(""),
        Line::from(Span::styled(
            "  Agents are reached over SSH, and Railway only answers",
            body,
        )),
        Line::from(Span::styled(
            "  keys it knows. Registered once, it covers every agent.",
            body,
        )),
        Line::from(""),
        // The footers' badge chords, centered: the question's answers are the
        // card's focal point, not another row of copy.
        Line::from(vec![
            chord_badge(theme, "y"),
            Span::styled(" Yes — register this key", body),
            Span::raw("    "),
            chord_badge(theme, "n"),
            Span::styled(" No, not now", body),
        ])
        .alignment(Alignment::Center),
    ];
    let width = (55 + DIALOG_CHROME_X).min(area.width.saturating_sub(4));
    let height = (lines.len() as u16 + DIALOG_CHROME_Y).min(area.height.saturating_sub(2));
    let panel = centered(width, height, area);
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(lines).block(
            dialog_block(theme).title(Span::styled(
                " Register your SSH key with Railway? ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
        ),
        panel,
    );
}

/// The corner confirmation, over whatever is underneath it.
///
/// Bottom right, clear of the key strip on the left of the same row and of the
/// `? keys` badge on its right — it sits a line above both.
fn render_toast(app: &App, f: &mut Frame) {
    let Some(toast) = app.toast.as_ref().filter(|toast| !toast.expired()) else {
        return;
    };
    let theme = app.theme;
    let area = page(f);
    let text = format!(" {}  {}  ", if toast.ok { "✓" } else { "✕" }, toast.text);
    let w = (text.chars().count() as u16 + DIALOG_CHROME_X).min(area.width);
    let h = 3.min(area.height);
    // Clear of the key strip on the last row and of the pane border above it,
    // so it floats inside the pane rather than colliding with its corner.
    let rect = Rect {
        x: area.right().saturating_sub(w + 2),
        y: area.bottom().saturating_sub(h + 2),
        width: w,
        height: h,
    };
    let accent = if toast.ok {
        theme.accent
    } else {
        theme.pending
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(Span::styled(text, Style::default().fg(theme.fg)))
            .block(dialog_block(theme).border_style(Style::default().fg(accent))),
        rect,
    );
}

fn render_screen(app: &App, f: &mut Frame, rects: &mut PaneRects) {
    match app.screen {
        Screen::Setup => {
            render_menu(app, f, rects);
            render_wizard(app, f);
        }
        Screen::Settings => {
            render_menu(app, f, rects);
            render_settings(app, f);
        }
        Screen::Menu => render_menu(app, f, rects),
        Screen::Manage => render_manage(app, f, rects),
        Screen::TargetPick => {
            render_menu(app, f, rects);
            render_target_pick(app, f);
        }
        Screen::AgentPick => {
            render_menu(app, f, rects);
            render_agent_pick(app, f);
        }
        Screen::HarnessPick => {
            render_manage(app, f, rects);
            render_harness_pick(app, f);
        }
        Screen::ManagePrompt => {
            render_manage(app, f, rects);
            render_manage_prompt(app, f);
        }
    }
}

/// A whole block, borders included — what a click may land on.
fn whole(area: Rect) -> PaneBox {
    PaneBox {
        x: area.x,
        y: area.y,
        w: area.width,
        h: area.height,
    }
}

/// The interior of a bordered block — what a selection may cover.
fn interior(area: Rect) -> PaneBox {
    PaneBox {
        x: area.x + 1,
        y: area.y + 1,
        w: area.width.saturating_sub(2),
        h: area.height.saturating_sub(2),
    }
}

fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn render_menu(app: &App, f: &mut Frame, rects: &mut PaneRects) {
    let theme = app.theme;
    let area = page(f);
    let big = area.width >= BANNER_W + 8 && area.height >= 26;
    let banner_h = if big { BANNER_H } else { 1 };
    // Text rows plus the border. Twice the writing room of the original two
    // rows — a prompt is a paragraph these days, not a title.
    let prompt_h = if area.height >= 30 { 6 } else { 2 } + DIALOG_CHROME_Y;
    // The room around the prompt lives outside its outline, not inside it.
    let prompt_gap = if area.height >= 30 { 2 } else { 1 };
    let panel_w = 74.min(area.width.saturating_sub(2)).max(40.min(area.width));
    let cards = app.cards();
    // Descriptions cost rows, and on a short screen those rows come out of the
    // prompt box. Names only, rather than a menu with no prompt on it.
    let chrome = banner_h + prompt_h + prompt_gap * 2 + 10;
    let mut block = card_block(&cards, panel_w as usize, true);
    if chrome + block.height(cards.len()) > area.height {
        block = card_block(&cards, panel_w as usize, false);
    }
    let cards_h = block.height(cards.len());
    let panel_h = chrome + cards_h;
    let panel = centered(panel_w, panel_h.min(area.height), area);

    let rows = Layout::vertical([
        Constraint::Length(banner_h),
        Constraint::Length(1),          // breathing room under the wordmark
        Constraint::Length(1),          // CLOUD AGENTS
        Constraint::Length(1),          // title
        Constraint::Length(1),          // subtitle
        Constraint::Length(prompt_gap), // the prompt's room, outside its outline
        Constraint::Length(prompt_h),
        Constraint::Length(prompt_gap), // and the same below
        Constraint::Length(cards_h),
        Constraint::Min(0),
        Constraint::Length(1), // target
        Constraint::Length(1), // gap
        Constraint::Length(1), // hint
    ])
    .split(panel);

    let wordmark = if big {
        Paragraph::new(banner_lines(theme))
    } else {
        Paragraph::new("RAILWAY CLOUD-AGENTS").style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    };
    f.render_widget(wordmark.alignment(Alignment::Center), rows[0]);
    if big {
        f.render_widget(
            // Fullwidth forms, so the line reads a size up from the body text
            // without a second block font to maintain.
            Paragraph::new("ＣＬＯＵＤ　ＡＧＥＮＴＳ")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.accent)),
            rows[2],
        );
    }
    f.render_widget(
        Paragraph::new("What should we build today?")
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)),
        rows[3],
    );
    // Only a status goes here. The line that used to explain what the prompt
    // was for said nothing the prompt box does not already say.
    if !app.status.is_empty() {
        f.render_widget(
            Paragraph::new(app.status.clone())
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.accent)),
            rows[4],
        );
    }

    render_prompt(app, f, rows[6]);
    rects.prompt = whole(rows[6]);
    render_cards(app, f, rows[8], &block, rects);

    // Where the prompt lands, on its own line above the keys. It was a chip in
    // the prompt box, which put the least-changed setting in the busiest place
    // on the screen.
    f.render_widget(
        Paragraph::new(target_line(app)).alignment(Alignment::Center),
        rows[10],
    );

    let chords: &[(&str, &str)] = match app.menu_focus {
        MenuFocus::Prompt => &[
            ("enter", "launch"),
            ("shift+tab", "agent"),
            ("⌥s", "settings"),
        ],
        MenuFocus::Cards => &[
            ("↑↓", "select"),
            ("enter", "open"),
            ("⌥s", "settings"),
            ("q", "quit"),
        ],
    };
    f.render_widget(
        Paragraph::new(Line::from(chord_spans(theme, chords))).alignment(Alignment::Center),
        rows[12],
    );
}

/// `^t  Target Project  name (environment)`, or an invitation to set one.
/// The shortcut sits right on the field it changes rather than in the
/// footer's own chord list, which otherwise says nothing about what "target"
/// even refers to.
fn target_line(app: &App) -> Line<'static> {
    let theme = app.theme;
    let mut spans = vec![
        chord_badge(theme, "^t"),
        Span::raw(" "),
        Span::styled(
            "Target Project  ",
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ),
    ];
    spans.push(match app.target.as_ref() {
        Some(target) => Span::styled(
            format!("{} ({})", target.project_name, target.environment_name),
            Style::default().fg(theme.accent),
        ),
        None => Span::styled("not set", Style::default().fg(theme.pending)),
    });
    Line::from(spans)
}

/// The wait, with the task in front of you.
///
/// The step list is a fixed height and the panel is centred once: a list that
/// grew with each step would shove everything above it up the screen, which
/// reads as flicker rather than progress. Steps wrap instead of being clipped —
/// several of them are full sentences, and a truncated one is worse than no
/// line at all.
fn render_loading(app: &App, f: &mut Frame, area: Rect) {
    let theme = app.theme;
    let loading = &app.loading;

    // The pane it is about to become: same border, same title bar, so the
    // session appearing in it reads as the same thing finishing rather than a
    // different screen replacing it.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(" {} · starting ", loading.harness),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let area = {
        let inner = block.inner(area);
        f.render_widget(block, area);
        inner
    };

    const STEP_ROWS: u16 = 9;
    let task_h = match loading.prompt.as_deref() {
        // Three lines of task plus its border: a prompt is a sentence, and the
        // whole point of showing it is not making the user wonder what is
        // starting.
        Some(_) => 5,
        None => 0,
    };
    // Size the panel to its content and centre *that*, rather than letting it
    // span the pane: the steps read as a left-aligned block, and a block the
    // full width of the pane is a block pinned to its left border. The widest
    // line decides, so the group stays centred as steps arrive.
    let content_w = loading
        .steps
        .iter()
        .map(|step| step.chars().count() + 2)
        .chain(std::iter::once(loading.target.chars().count()))
        // The task is deliberately absent: it is a whole sentence, and letting
        // it set the width stretched the panel across the pane and pushed the
        // steps out to the left margin. It wraps inside a fixed box instead.
        .max()
        .unwrap_or(0)
        // A floor only so an empty panel is not a sliver; anything larger pads
        // the panel past its content and the centring visibly drifts left.
        .clamp(20, area.width.max(1) as usize) as u16;
    let panel = centered(content_w, (STEP_ROWS + task_h + 4).min(area.height), area);
    let rows = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // target
        Constraint::Length(1), // gap
        Constraint::Length(task_h),
        Constraint::Length(STEP_ROWS),
        Constraint::Min(0),
        Constraint::Length(1), // hint
    ])
    .split(panel);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", spinner_frame(loading.tick)),
                Style::default().fg(theme.accent),
            ),
            Span::styled(
                loading.target.clone(),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ]))
        .alignment(Alignment::Center),
        rows[0],
    );
    f.render_widget(
        Paragraph::new("preparing the agent")
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme.dim)),
        rows[1],
    );

    if let Some(prompt) = loading.prompt.as_deref() {
        // A third of the pane, centred: a fixed frame the task wraps inside,
        // rather than a frame the task drags open.
        let task_area = centered((area.width / 3).max(24), task_h, rows[3]);
        f.render_widget(
            Paragraph::new(prompt.to_string())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(theme.accent_dim))
                        .title(Span::styled(" Task ", Style::default().fg(theme.dim))),
                )
                .style(Style::default().fg(theme.fg))
                .wrap(Wrap { trim: true }),
            task_area,
        );
    }

    f.render_widget(
        Paragraph::new(step_lines(app, rows[4].width)).wrap(Wrap { trim: false }),
        rows[4],
    );
}

/// Braille spinner, one frame per tick.
fn spinner_frame(tick: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[tick % FRAMES.len()]
}

/// Steps as lines: everything finished is ticked and dimmed, the newest is
/// live. Only the tail is kept, so the block never outgrows its box even on a
/// launch that reports a dozen things.
fn step_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let theme = app.theme;
    let steps = &app.loading.steps;
    const STEP_ROWS: usize = 9;

    // Each wrapped step costs more than one row; budget by estimated height so
    // the tail that is kept actually fits.
    let usable = width.saturating_sub(2).max(20) as usize;
    let mut budget = STEP_ROWS;
    let mut start = steps.len();
    for (i, step) in steps.iter().enumerate().rev() {
        let rows = step.chars().count().div_ceil(usable).max(1);
        if rows > budget {
            break;
        }
        budget -= rows;
        start = i;
    }

    let last = steps.len().saturating_sub(1);
    steps[start..]
        .iter()
        .enumerate()
        .map(|(offset, step)| {
            let i = start + offset;
            let (marker, style) = if i == last {
                (
                    format!("{} ", spinner_frame(app.loading.tick)),
                    Style::default().fg(theme.accent),
                )
            } else {
                ("✓ ".to_string(), Style::default().fg(theme.dim))
            };
            Line::from(vec![
                Span::styled(marker, style),
                Span::styled(step.clone(), style),
            ])
        })
        .collect()
}

fn render_prompt(app: &App, f: &mut Frame, area: Rect) {
    let theme = app.theme;
    let focused = app.menu_focus == MenuFocus::Prompt;
    let empty = app.prompt.is_empty();
    let (text, fg) = if empty && !focused {
        (
            "Fix a bug, scaffold a service, explain a repo…".to_string(),
            theme.dim,
        )
    } else if focused {
        (format!("{}▏", app.prompt), theme.fg)
    } else {
        (app.prompt.clone(), theme.fg)
    };

    // Only the harness. Where it lands is on its own line under the cards —
    // it changes rarely, and it was crowding the one control being used.
    let count = if empty {
        String::new()
    } else {
        format!(" {} ", app.prompt.chars().count())
    };

    f.render_widget(Clear, area);

    let block = dialog_block(theme)
        .border_style(Style::default().fg(if focused {
            theme.accent
        } else {
            theme.accent_dim
        }))
        .title(Span::styled(
            " Prompt ",
            Style::default()
                .fg(if focused { theme.accent } else { theme.dim })
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(vec![
            Span::styled(
                format!(" {} ", app.harness_name()),
                Style::default()
                    .fg(if focused { theme.accent } else { theme.fg })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("shift+tab ", Style::default().fg(theme.dim)),
        ]))
        .title_bottom(
            Line::from(Span::styled(count, Style::default().fg(theme.dim))).right_aligned(),
        );

    // Keep the cursor line in view once the wrapped text outgrows the box.
    // Without this the box simply stops showing what is being typed, which is
    // the one thing it exists to do.
    let inner_w = area.width.saturating_sub(DIALOG_CHROME_X).max(1) as usize;
    let inner_h = area.height.saturating_sub(DIALOG_CHROME_Y).max(1) as usize;
    let scroll_y = wrapped_lines(&text, inner_w).saturating_sub(inner_h) as u16;

    f.render_widget(
        Paragraph::new(text)
            .block(block)
            .style(Style::default().fg(fg))
            .wrap(Wrap { trim: false })
            .scroll((scroll_y, 0)),
        area,
    );
}

/// How many rows `text` occupies once wrapped at `width`, breaking on spaces
/// the way ratatui does and hard-wrapping a word that cannot fit.
fn wrapped_lines(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut rows = 1usize;
    let mut column = 0usize;
    for word in text.split_inclusive(' ') {
        let len = word.chars().count();
        if column + len > width && column > 0 {
            rows += 1;
            column = 0;
        }
        if len > width {
            rows += (len - 1) / width;
            column = len % width;
        } else {
            column += len;
        }
    }
    rows
}

/// The menu's cards, centred as a block under the prompt.
///
/// Centred as a block, not line by line: each card is a name in a fixed column
/// followed by a sentence of a different length, so centring them individually
/// would leave the names in a ragged column down the middle.
/// The cards, laid out: how they wrap and how far in the block starts.
///
/// Computed before the layout as well as during the draw, because the number
/// of rows the cards need decides how much room the menu gives them.
struct CardBlock {
    /// Wrapped description lines, one list per card. Empty when the terminal is
    /// too narrow to carry the sentences at all.
    descriptions: Vec<Vec<String>>,
    indent: usize,
}

impl CardBlock {
    /// One line per description line, at least one per card, plus a blank
    /// between cards.
    fn height(&self, cards: usize) -> u16 {
        let text: usize = match self.descriptions.is_empty() {
            true => cards,
            false => self.descriptions.iter().map(|d| d.len().max(1)).sum(),
        };
        (text + cards) as u16
    }
}

/// Fit the cards to `width`.
///
/// Descriptions wrap into the column beside the name rather than widening the
/// block: the cards belong under the prompt box, and a block wider than the box
/// reads as a second, competing column. Where a wrapped sentence would be
/// shredded — or where the screen is too short to hold the extra rows, which
/// the caller decides with `descriptions` — the names stand alone.
fn card_block(
    cards: &[(&'static str, &'static str)],
    width: usize,
    descriptions: bool,
) -> CardBlock {
    let names = || CardBlock {
        descriptions: Vec::new(),
        indent: width
            .saturating_sub(
                cards
                    .iter()
                    .map(|(label, _)| 2 + label.chars().count())
                    .max()
                    .unwrap_or(0),
            )
            .div_euclid(2),
    };

    let desc_w = width.saturating_sub(LABEL_W + CARD_GUTTER);
    if !descriptions || desc_w < 24 {
        return names();
    }
    let descriptions: Vec<Vec<String>> = cards
        .iter()
        .map(|(_, desc)| wrap_words(desc, desc_w))
        .collect();
    let content = LABEL_W
        + CARD_GUTTER
        + descriptions
            .iter()
            .flatten()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
    CardBlock {
        descriptions,
        indent: width.saturating_sub(content) / 2,
    }
}

/// Break `text` on spaces at `width`, hard-splitting nothing — a word longer
/// than the column simply overhangs, which cannot happen with the copy here and
/// is better than a name broken in half if it ever does.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut column = 0usize;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if column > 0 && column + 1 + len > width {
            lines.push(std::mem::take(&mut line));
            column = 0;
        }
        if column > 0 {
            line.push(' ');
            column += 1;
        }
        line.push_str(word);
        column += len;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn render_cards(app: &App, f: &mut Frame, area: Rect, block: &CardBlock, rects: &mut PaneRects) {
    let theme = app.theme;
    let cards = app.cards();
    let indent = " ".repeat(block.indent);
    rects.cards = Default::default();

    let mut lines: Vec<Line> = Vec::new();
    for (i, (label, _)) in cards.iter().enumerate() {
        // The card's own rows, not the blank after it: a click in the gap
        // between two cards should pick neither.
        if let Some(slot) = rects.cards.get_mut(i) {
            let height = block
                .descriptions
                .get(i)
                .map(|d| d.len().max(1))
                .unwrap_or(1);
            *slot = PaneBox {
                x: area.x,
                y: area.y + lines.len() as u16,
                w: area.width,
                h: height as u16,
            };
        }
        let on = app.menu_focus == MenuFocus::Cards && i == app.card;
        let (fg, bg) = if on {
            (theme.on_accent, theme.accent)
        } else {
            (theme.fg, Color::Reset)
        };
        let desc = block.descriptions.get(i);
        let dim = Style::default().fg(if on { theme.fg } else { theme.dim });

        lines.push(Line::from(vec![
            Span::raw(indent.clone()),
            Span::styled(
                if on { "▌" } else { " " },
                Style::default().fg(theme.accent),
            ),
            Span::styled(
                match desc.is_some() {
                    true => format!(" {label:<LABEL_W$}"),
                    false => format!(" {label}"),
                },
                Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                match desc.and_then(|lines| lines.first()) {
                    Some(first) => format!(" {first}"),
                    None => String::new(),
                },
                dim,
            ),
        ]));
        // Continuations line up under the first line of the description, not
        // under the name — the two columns stay two columns.
        for line in desc.map(|d| &d[1.min(d.len())..]).unwrap_or(&[]) {
            lines.push(Line::from(vec![
                Span::raw(format!(
                    "{indent}{:width$}",
                    "",
                    width = LABEL_W + CARD_GUTTER
                )),
                Span::styled(line.clone(), dim),
            ]));
        }
        lines.push(Line::from(""));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The size the session pane will have, given the whole terminal — the same
/// arithmetic `render_manage` does, so the emulator and the pane agree.
/// `None` when there is no room for two panes.
pub fn session_pane_size(
    area: Option<ratatui::layout::Size>,
    maximized: bool,
) -> Option<(u16, u16)> {
    let area = area?;
    // The same inset the renderer applies — see `page_size` for why the two
    // must never disagree.
    let (width, height) = page_size(area.width, area.height);
    // Maximized there is no tree to leave room for, so no minimum width to
    // meet either.
    if !maximized && width < 70 {
        return None;
    }
    // Rows: header, gap, panes, hint. Columns: the tree, then what is left.
    // Both minus the pane's own border.
    let rows = height.saturating_sub(3).saturating_sub(2).max(1);
    let tree = if maximized { 0 } else { TREE_W };
    let cols = width.saturating_sub(tree).saturating_sub(2).max(1);
    Some((rows, cols))
}

fn render_manage(app: &App, f: &mut Frame, rects: &mut PaneRects) {
    let theme = app.theme;
    let area = page(f);
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Length(1), // gap
        Constraint::Min(3),    // panes
        Constraint::Length(1), // hint
    ])
    .split(area);

    let rows = app.rows();
    let header = Line::from(vec![
        Span::styled(
            " RAILWAY CLOUD-AGENTS ",
            Style::default()
                .fg(theme.on_accent)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if app.status.is_empty() {
                String::new()
            } else {
                format!("  ·  {}", app.status)
            },
            Style::default().fg(theme.dim),
        ),
    ]);
    f.render_widget(Paragraph::new(header), chunks[0]);

    // Detail is fixed-width so the tree keeps the space it needs on a narrow
    // terminal; below that there is no room for two panes at all.
    // The tree is a fixed, narrow column and the right pane takes everything
    // else — the detail (and, later, a live session) is what you are actually
    // looking at, and a tree that grows with the window just pads names with
    // whitespace.
    // ⌥f hands the whole width to the session: the tree is navigation, and
    // once you are working in a session there is nothing to navigate.
    let full = app.maximized && app.active_session().is_some();
    let two_pane = !full && chunks[2].width >= 70;
    let panes = if two_pane {
        Layout::horizontal([Constraint::Length(TREE_W), Constraint::Min(20)]).split(chunks[2])
    } else {
        Layout::horizontal([Constraint::Min(0)]).split(chunks[2])
    };

    if full {
        let pane = panes[0];
        rects.session = interior(pane);
        rects.session_outer = whole(pane);
        rects.tree = PaneBox::default();
        rects.tree_outer = PaneBox::default();
        if let Some(session) = app.active_session() {
            render_session(app, session, f, pane);
        }
        render_manage_footer(app, f, chunks[3], rects);
        return;
    }

    let tree_focused = app.focus == ManageFocus::Tree;

    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| ListItem::new(tree_line(theme, r)))
        .collect();
    let mut state = ListState::default();
    state.select(if rows.is_empty() {
        None
    } else {
        Some(app.cursor)
    });
    f.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(if tree_focused {
                        theme.accent
                    } else {
                        theme.accent_dim
                    }))
                    .title(Span::styled(
                        " cloud agents ",
                        Style::default().fg(theme.dim),
                    )),
            )
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(theme.selection),
            ),
        panes[0],
        &mut state,
    );
    rects.tree = interior(panes[0]);
    rects.tree_outer = whole(panes[0]);

    if two_pane {
        rects.session = interior(panes[1]);
        rects.session_outer = whole(panes[1]);
        // What the right pane shows follows the selection, not merely whether a
        // session happens to be open: standing on an agent should show that
        // agent's cards even while one of its sessions is running in the
        // background. Typing in a session is the exception — the pane it has
        // the keyboard in cannot vanish from under it.
        let show_session = app.focus == ManageFocus::Session
            || matches!(
                app.selected_row().map(|row| row.kind),
                Some(RowKind::Session(..))
            );
        if app.loading.active {
            render_loading(app, f, panes[1]);
        } else {
            match app.active_session().filter(|_| show_session) {
                Some(session) => render_session(app, session, f, panes[1]),
                None => f.render_widget(
                    Paragraph::new(detail_lines(app)).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(theme.accent_dim))
                            .title(Span::styled(" agent ", Style::default().fg(theme.dim))),
                    ),
                    panes[1],
                ),
            }
        }
    }

    render_manage_footer(app, f, chunks[3], rects);
}

/// The bottom line of the Manage screen — a held confirmation, or the keys that
/// apply right now — plus the selection painted over the panes above it.
///
/// Shared with the maximized layout, which has no tree to draw but the same
/// footer and the same drag-to-copy.
fn render_manage_footer(app: &App, f: &mut Frame, area: Rect, rects: &PaneRects) {
    let theme = app.theme;
    // A held action replaces the hint line: it is the only thing that matters
    // until it is answered, and it must not be missable.
    if let Some(confirm) = app.confirm.as_ref() {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " confirm ",
                    Style::default()
                        .fg(theme.on_accent)
                        .bg(theme.pending)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", confirm.question()),
                    Style::default().fg(theme.fg),
                ),
            ])),
            area,
        );
        return;
    }

    // The selection is painted last, straight onto the buffer: it has to sit
    // over the pane's own colours, and only inside the pane it started in.
    if let Some(selection) = app.selection.filter(|s| !s.is_empty()) {
        let bounds = match selection.pane {
            ManageFocus::Tree => rects.tree,
            ManageFocus::Session => rects.session,
        };
        let spans = selection.spans(bounds);
        let buffer = f.buffer_mut();
        for (y, x0, x1) in spans {
            for x in x0..=x1 {
                buffer[(x, y)].set_style(
                    Style::default()
                        .bg(theme.selection)
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
    }

    // The actions that apply here, and nothing else. The old strip listed
    // everything the screen could do at all times, which is a lot to read past
    // to find the one you wanted; the rest lives behind `?`.
    let sleeping = app
        .selected_agent_status()
        .is_some_and(|status| status != "running");
    let hint: Vec<(&str, &str)> = if app.maximized {
        vec![("⌥f", "restore the tree"), ("⌥/⇧esc / ^]", "stop typing")]
    } else if app.focus == ManageFocus::Session {
        let mut keys = vec![("⌥/⇧esc / ^]", "stop typing"), ("⌥f", "maximize")];
        // The agent is taking the clicks, so say how to take one back — this is
        // the terminal's own convention, but nobody guesses it.
        if app.active_session().is_some_and(|s| s.wants_mouse()) {
            keys.push(("shift+drag", "select"));
        }
        keys
    } else {
        match app.selected_row().map(|r| r.kind) {
            Some(RowKind::Session(..)) => vec![
                ("enter", "connect"),
                ("⌥f", "maximize"),
                ("⌥enter", "full screen"),
                ("c", "copy ssh"),
                ("x", "end session"),
                if sleeping {
                    ("w", "wake")
                } else {
                    ("s", "sleep")
                },
                ("d", "delete agent"),
            ],
            Some(RowKind::Agent(..)) => vec![
                ("enter", "connect"),
                ("n", "new session"),
                if sleeping {
                    ("w", "wake")
                } else {
                    ("s", "sleep")
                },
                ("d", "delete agent"),
            ],
            // A group is a place, not a thing to open: the keys that matter
            // are the ones that act on the environment it stands for.
            Some(RowKind::Group(..)) => vec![
                ("n", "new agent here"),
                ("t", "target"),
                ("shift+r", "find agents"),
            ],
            _ => vec![
                ("enter", "open"),
                ("n", "new agent"),
                ("shift+r", "find agents"),
            ],
        }
    };
    // Only worth advertising once there is somewhere to cycle to; on a single
    // pane the chord is a no-op and the hint would just be a lie.
    let mut hint = hint;
    if app.sessions.len() > 1 {
        hint.push(("⌥[ ⌥]", "switch session"));
    }
    let spans = chord_spans(theme, &hint);
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    // Help sits on the far right, out of the way of the actions and always in
    // the same place — drawn second so it wins if the row ever fills up.
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " ? ",
                Style::default()
                    .fg(theme.on_accent)
                    .bg(theme.accent_dim)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" keys ", Style::default().fg(theme.dim)),
        ]))
        .alignment(Alignment::Right),
        area,
    );

    if app.keys_open {
        render_keys(app, f);
    }
}

/// One row of a card panel: a name, an optional dim tag beside it, and an
/// optional line of explanation under it.
struct PanelRow {
    label: String,
    tag: String,
    detail: String,
}

/// The centred card list both the setup flow and the target chooser are made
/// of. One shape, so choosing a target looks like answering the same question
/// setup asks — because it is.
struct Panel<'a> {
    title: &'a str,
    heading: &'a str,
    /// Progress dots: (index, total). `None` draws no dots.
    position: Option<(usize, usize)>,
    rows: &'a [PanelRow],
    cursor: usize,
    footer: Line<'static>,
}

fn render_panel(f: &mut Frame, theme: &Theme, area: Rect, panel: Panel) {
    let body_h = panel
        .rows
        .iter()
        .map(|row| if row.detail.is_empty() { 1 } else { 2 })
        .sum::<usize>() as u16;
    // No progress dots means no row held open for them.
    let dots_h = u16::from(panel.position.is_some());
    let width = (62 + DIALOG_CHROME_X).min(area.width.saturating_sub(4));
    let height = (body_h + dots_h + 5 + DIALOG_CHROME_Y).min(area.height.saturating_sub(2));
    let outer = centered(width, height, area);
    f.render_widget(Clear, outer);

    let block = dialog_block(theme).title(Span::styled(
        format!(" {} ", panel.title),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(outer);
    f.render_widget(block, outer);

    let rows = Layout::vertical([
        Constraint::Length(1), // heading
        Constraint::Length(dots_h),
        Constraint::Length(1), // gap
        Constraint::Length(body_h),
        Constraint::Min(0),
        Constraint::Length(1), // footer
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(panel.heading.to_string())
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)),
        rows[0],
    );

    // Dots rather than "step 2 of 4": the shape of the flow at a glance.
    if let Some((index, total)) = panel.position {
        let dots: Vec<Span> = (0..total)
            .map(|i| {
                Span::styled(
                    if i == index { "●  " } else { "○  " },
                    Style::default().fg(if i == index {
                        theme.accent
                    } else {
                        theme.accent_dim
                    }),
                )
            })
            .collect();
        f.render_widget(
            Paragraph::new(Line::from(dots)).alignment(Alignment::Center),
            rows[1],
        );
    }

    // The card clamps to the terminal, so a long list (a real account's
    // projects) can hold more rows than the body has lines. Window the rows
    // to keep the cursor visible: walk the start of the window forward until
    // everything from there through the cursor fits.
    let avail = rows[3].height as usize;
    let row_height = |row: &PanelRow| if row.detail.is_empty() { 1 } else { 2 };
    let mut first = 0usize;
    while first < panel.cursor
        && panel.rows[first..=panel.cursor.min(panel.rows.len() - 1)]
            .iter()
            .map(row_height)
            .sum::<usize>()
            > avail
    {
        first += 1;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(panel.rows.len() * 2);
    for (i, row) in panel.rows.iter().enumerate().skip(first) {
        let on = i == panel.cursor;
        let mut spans = vec![
            Span::styled(
                if on { "▌ " } else { "  " },
                Style::default().fg(theme.accent),
            ),
            Span::styled(
                row.label.clone(),
                if on {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg)
                },
            ),
        ];
        if !row.tag.is_empty() {
            spans.push(Span::styled(
                format!("  {}", row.tag),
                // The tag steps forward with its row: on the settings card it
                // is the current value, which is the thing being changed.
                Style::default().fg(if on { theme.fg } else { theme.dim }),
            ));
        }
        lines.push(Line::from(spans));
        // Only when there is something to say. An empty description line turns
        // a list of names into a list with gaps in it.
        if !row.detail.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", row.detail),
                Style::default().fg(theme.dim),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), rows[3]);
    f.render_widget(
        Paragraph::new(panel.footer).alignment(Alignment::Center),
        rows[5],
    );
}

fn render_wizard(app: &App, f: &mut Frame) {
    let Some(wizard) = app.wizard.as_ref() else {
        return;
    };
    let theme = app.theme;
    let rows: Vec<PanelRow> = wizard
        .options()
        .into_iter()
        .map(|(label, detail)| PanelRow {
            label,
            tag: String::new(),
            detail,
        })
        .collect();

    let footer = if let Some(busy) = wizard.busy.as_deref() {
        Line::from(vec![
            Span::styled(
                format!("{} ", spinner_frame(app.loading.tick)),
                Style::default().fg(theme.accent),
            ),
            Span::styled(busy.to_string(), Style::default().fg(theme.fg)),
        ])
    } else if let Some(error) = wizard.error.as_deref() {
        Line::from(Span::styled(
            format!("  {error}"),
            Style::default().fg(theme.pending),
        ))
    } else {
        Line::from(chord_spans(
            theme,
            &[("↑↓", "choose"), ("enter", "next"), ("esc", "back")],
        ))
    };

    render_panel(
        f,
        theme,
        page(f),
        Panel {
            title: "setup",
            heading: wizard.title(),
            position: wizard.position(),
            rows: &rows,
            cursor: wizard.cursor,
            footer,
        },
    );
}

/// The ⌥s settings card: every preference with its current value beside it,
/// or the project sub-picker while it is open.
fn render_settings(app: &App, f: &mut Frame) {
    let Some(settings) = app.settings.as_ref() else {
        return;
    };
    let theme = app.theme;

    // The sub-picker replaces the card wholesale, like a wizard step.
    if let Some(pick) = settings.pick {
        let rows: Vec<PanelRow> = settings
            .picker_options()
            .into_iter()
            .map(|(label, tag, detail)| PanelRow { label, tag, detail })
            .collect();
        let footer = if let Some(busy) = settings.busy.as_deref() {
            Line::from(vec![
                Span::styled(
                    format!("{} ", spinner_frame(app.loading.tick)),
                    Style::default().fg(theme.accent),
                ),
                Span::styled(busy.to_string(), Style::default().fg(theme.fg)),
            ])
        } else if let Some(error) = settings.error.as_deref() {
            Line::from(Span::styled(
                format!("  {error}"),
                Style::default().fg(theme.pending),
            ))
        } else {
            Line::from(chord_spans(
                theme,
                &[("↑↓", "choose"), ("enter", "set default"), ("esc", "back")],
            ))
        };
        render_panel(
            f,
            theme,
            page(f),
            Panel {
                title: "settings",
                heading: "Where should agents live?",
                position: None,
                rows: &rows,
                cursor: pick,
                footer,
            },
        );
        return;
    }

    let cycles = settings.cycles();
    let rows: Vec<PanelRow> = settings
        .options()
        .into_iter()
        .enumerate()
        .map(|(i, (label, value, detail))| PanelRow {
            // Padded so the values read as a column.
            label: format!("{label:<19}"),
            // The highlighted value grows arrows when ←/→ changes it in
            // place — the hint that this row edits right here.
            tag: if i == settings.cursor && cycles {
                format!("‹ {value} ›")
            } else {
                value
            },
            detail,
        })
        .collect();
    let footer = Line::from(chord_spans(
        theme,
        &[
            ("↑↓", "choose"),
            ("←→", "change"),
            ("enter", "edit"),
            ("esc", "close"),
        ],
    ));
    render_panel(
        f,
        theme,
        page(f),
        Panel {
            title: "settings",
            heading: "Cloud agent settings",
            position: None,
            rows: &rows,
            cursor: settings.cursor,
            footer,
        },
    );
}

/// Choosing which agent a new session goes on. Only drawn when there is more
/// than one to choose between.
fn render_agent_pick(app: &App, f: &mut Frame) {
    let Some(picker) = app.agent_pick.as_ref() else {
        return;
    };
    let theme = app.theme;
    let rows: Vec<PanelRow> = picker
        .rows()
        .into_iter()
        .map(|(label, tag)| PanelRow {
            label,
            tag,
            detail: String::new(),
        })
        .collect();
    let footer = Line::from(chord_spans(
        theme,
        &[
            ("↑↓", "choose"),
            ("enter", "new session"),
            ("esc", "cancel"),
        ],
    ));

    render_panel(
        f,
        theme,
        page(f),
        Panel {
            title: "new session",
            heading: "Which cloud agent?",
            position: None,
            rows: &rows,
            cursor: picker.cursor,
            footer,
        },
    );
}

/// ⌥n's picker: which agent runs the new session. The wizard's agent step,
/// floated over the tree the launch is aimed at.
fn render_harness_pick(app: &App, f: &mut Frame) {
    let Some(cursor) = app.harness_pick else {
        return;
    };
    let theme = app.theme;
    let rows: Vec<PanelRow> = crate::commands::cloud_agent::tui::app::HARNESSES
        .iter()
        .map(|slug| PanelRow {
            label: (*slug).to_string(),
            tag: String::new(),
            detail: super::wizard::harness_blurb(slug).to_string(),
        })
        .collect();
    let footer = Line::from(chord_spans(
        theme,
        &[
            ("↑↓", "choose"),
            ("enter", "new session"),
            ("esc", "cancel"),
        ],
    ));

    render_panel(
        f,
        theme,
        page(f),
        Panel {
            title: "new session",
            heading: "Which agent should run it?",
            position: None,
            rows: &rows,
            cursor,
            footer,
        },
    );
}

/// ⌥p's composer: the menu's prompt box, floated over the tree so a new
/// request doesn't cost the walk back to the menu.
fn render_manage_prompt(app: &App, f: &mut Frame) {
    let Some(draft) = app.manage_prompt.as_ref() else {
        return;
    };
    let theme = app.theme;
    let area = page(f);
    let outer = centered(
        (62 + DIALOG_CHROME_X).min(area.width.saturating_sub(4)),
        // Three rows to type in, plus the border and the padding around them.
        3 + DIALOG_CHROME_Y,
        area,
    );
    f.render_widget(Clear, outer);

    let block = dialog_block(theme)
        .title(Span::styled(
            " New Session ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(vec![
            Span::styled(
                format!(" {} ", app.harness_name()),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("shift+tab ", Style::default().fg(theme.dim)),
        ]))
        .title_bottom(
            Line::from(Span::styled(
                " enter send · esc close ",
                Style::default().fg(theme.dim),
            ))
            .right_aligned(),
        );

    let text = format!("{draft}▏");
    // Keep the cursor line in view once the wrapped text outgrows the box,
    // same as the menu's prompt. The padding is chrome, not writing room, so
    // it comes off the width and the height the text gets.
    let inner_w = outer.width.saturating_sub(DIALOG_CHROME_X).max(1) as usize;
    let inner_h = outer.height.saturating_sub(DIALOG_CHROME_Y).max(1) as usize;
    let scroll_y = wrapped_lines(&text, inner_w).saturating_sub(inner_h) as u16;

    f.render_widget(
        Paragraph::new(text)
            .block(block)
            .style(Style::default().fg(theme.fg))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((scroll_y, 0)),
        outer,
    );
}

/// Choosing where the prompt lands. The setup flow's project card, minus the
/// rest of the flow.
fn render_target_pick(app: &App, f: &mut Frame) {
    let Some(picker) = app.target_pick.as_ref() else {
        return;
    };
    let theme = app.theme;
    let rows: Vec<PanelRow> = picker
        .rows(app.default_project.as_deref())
        .into_iter()
        .map(|(label, tag)| PanelRow {
            label,
            tag,
            detail: String::new(),
        })
        .collect();
    let footer = if rows.is_empty() {
        Line::from(Span::styled(
            "No projects to pick from",
            Style::default().fg(theme.dim),
        ))
    } else {
        Line::from(chord_spans(
            theme,
            &[("↑↓", "choose"), ("enter", "set target"), ("esc", "cancel")],
        ))
    };

    render_panel(
        f,
        theme,
        page(f),
        Panel {
            title: "target",
            heading: "Where should Cloud Agents run?",
            position: None,
            rows: &rows,
            cursor: picker.cursor,
            footer,
        },
    );
}

/// The full key list, over the middle of the screen. A look-up rather than a
/// mode: the next keypress dismisses it.
fn render_keys(app: &App, f: &mut Frame) {
    let theme = app.theme;
    let area = page(f);

    let chord_w = KEY_HELP
        .iter()
        .flat_map(|(_, keys)| keys.iter().map(|(chord, _)| chord.chars().count()))
        .max()
        .unwrap_or(8);
    let mut lines: Vec<Line> = Vec::new();
    for (group, keys) in KEY_HELP {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            (*group).to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        for (chord, what) in *keys {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{chord:>chord_w$}  "),
                    Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled((*what).to_string(), Style::default().fg(theme.dim)),
            ]));
        }
    }

    let rows = lines.len() as u16;
    // As wide as the widest line it has to carry, so nothing gets truncated.
    let content_w = lines
        .iter()
        .map(|line| line.width() as u16)
        .max()
        .unwrap_or(48);
    let width = (content_w + DIALOG_CHROME_X).min(area.width.saturating_sub(4));
    let height = (rows + DIALOG_CHROME_Y).min(area.height.saturating_sub(2));
    let panel = centered(width, height, area);
    f.render_widget(Clear, panel);
    f.render_widget(
        Paragraph::new(lines).block(
            dialog_block(theme)
                .title(Span::styled(
                    " keys ",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ))
                .title_bottom(Line::from(Span::styled(
                    " any key closes ",
                    Style::default().fg(theme.dim),
                ))),
        ),
        panel,
    );
}

/// Draw the session's emulated screen.
///
/// Cell by cell, coalescing runs that share a style — a `Span` per cell would
/// be correct and unbearably slow at eighty columns times forty rows, several
/// times a second.
fn render_session(app: &App, session: &super::session::Session, f: &mut Frame, area: Rect) {
    let theme = app.theme;
    let focused = app.focus == ManageFocus::Session;
    let title = format!(" {} · {} ", session.agent_name, session.durable_name);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused {
            theme.accent
        } else {
            theme.accent_dim
        }))
        .title(Span::styled(
            title,
            Style::default()
                .fg(if focused { theme.accent } else { theme.dim })
                .add_modifier(Modifier::BOLD),
        ))
        // Only what the footer cannot say: the state of this pane's own
        // scrollback. The way out of a focused session is a key, and the key
        // strip at the bottom of the screen already has it.
        .title_bottom(Line::from(Span::styled(
            if session.ended() {
                " session ended "
            } else if session.stalled() {
                " no response "
            } else if session.scrolled_back() {
                " scrolled back · type to return "
            } else if !session.scrollable() {
                " no scrollback here "
            } else if focused {
                ""
            } else {
                " click or enter to type "
            },
            Style::default().fg(theme.dim),
        )));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // An attach gone silent has nothing to draw, so say what the silence
    // means instead of showing an empty screen. The platform can list a
    // session as running after its agent slept killed the process; attaching
    // to that name streams nothing, ever.
    if session.stalled() {
        let dim = Style::default().fg(theme.dim);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("Nothing has arrived from this session.", dim)),
                Line::from(Span::styled(
                    "It may have ended when the agent last slept —",
                    dim,
                )),
                Line::from(Span::styled(
                    "x closes this pane, n starts a fresh session.",
                    dim,
                )),
            ])
            .alignment(ratatui::layout::Alignment::Center),
            inner,
        );
        return;
    }

    let Some(lines) = session.with_screen(|screen| screen_lines(screen, focused)) else {
        return;
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// Convert one emulated screen into styled lines.
fn screen_lines(screen: &vt100::Screen, focused: bool) -> Vec<Line<'static>> {
    let (rows, cols) = screen.size();
    let (cursor_row, cursor_col) = screen.cursor_position();
    let mut out = Vec::with_capacity(rows as usize);

    for row in 0..rows {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut run_style: Option<Style> = None;

        for col in 0..cols {
            let (text, mut style) = match screen.cell(row, col) {
                Some(cell) => (
                    {
                        let c = cell.contents();
                        if c.is_empty() {
                            " ".to_string()
                        } else {
                            c.to_string()
                        }
                    },
                    cell_style(cell),
                ),
                None => (" ".to_string(), Style::default()),
            };
            // The cursor is drawn as a reversed cell, and only while the pane
            // has focus — two visible cursors would be a lie about where typing
            // goes.
            if focused && !screen.hide_cursor() && row == cursor_row && col == cursor_col {
                style = style.add_modifier(Modifier::REVERSED);
            }
            match run_style {
                Some(current) if current == style => run.push_str(&text),
                Some(current) => {
                    spans.push(Span::styled(std::mem::take(&mut run), current));
                    run.push_str(&text);
                    run_style = Some(style);
                }
                None => {
                    run.push_str(&text);
                    run_style = Some(style);
                }
            }
        }
        if let Some(style) = run_style {
            spans.push(Span::styled(run, style));
        }
        out.push(Line::from(spans));
    }
    out
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = convert_color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = convert_color(cell.bgcolor()) {
        style = style.bg(bg);
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

/// `Default` stays `None` so the terminal's own foreground and background show
/// through — the agent's palette should look like it does in a real terminal,
/// not be re-tinted by the theme.
fn convert_color(color: vt100::Color) -> Option<Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// What a session's state is, from this UI's point of view.
///
/// "connected" means this TUI has a pane on it. The platform's `attached` flag
/// answers a different question — whether *anyone* is attached, including
/// another terminal — and reporting that made the label flicker between
/// attached and running for no reason the user could see.
fn session_state(app: &App, name: &str, running: bool) -> &'static str {
    if !running {
        "exited"
    } else if app.sessions.iter().any(|pane| pane.durable_name == name) {
        "connected"
    } else {
        "running"
    }
}

/// Trim to `max` characters, with an ellipsis — a status card is one line.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

fn status_color(theme: &Theme, status: &str) -> Color {
    match status {
        "running" => theme.running,
        "sleeping" | "stopped" => theme.sleeping,
        _ => theme.pending,
    }
}

fn status_glyph(status: &str) -> &'static str {
    match status {
        "running" => "●",
        "sleeping" | "stopped" => "○",
        _ => "◌",
    }
}

fn tree_line(theme: &Theme, row: &Row) -> Line<'static> {
    let indent = "  ".repeat(row.depth);
    let mut spans = vec![Span::raw(indent)];

    match (&row.kind, row.expanded) {
        (RowKind::Agent(..), _) => {
            let status = row.status.clone().unwrap_or_default();
            spans.push(Span::styled(
                format!("{} ", status_glyph(&status)),
                Style::default().fg(status_color(theme, &status)),
            ));
            spans.push(Span::styled(
                row.label.clone(),
                Style::default().fg(theme.fg),
            ));
        }
        (RowKind::Session(..), _) => {
            // The marker is the state: a filled dot when this UI has it open,
            // a quiet branch when it is only running on the agent.
            let connected = row.status.is_some();
            spans.push(Span::styled(
                if connected { "● " } else { "↳ " },
                Style::default().fg(if connected { theme.running } else { theme.dim }),
            ));
            spans.push(Span::styled(
                row.label.clone(),
                Style::default().fg(theme.fg),
            ));
        }
        (RowKind::Separator, _) => spans.push(Span::styled(
            "─".repeat(TREE_W.saturating_sub(4) as usize),
            Style::default().fg(theme.accent_dim),
        )),
        (RowKind::Note(..) | RowKind::Hint, _) => spans.push(Span::styled(
            row.label.clone(),
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )),
        (_, Some(expanded)) => {
            spans.push(Span::styled(
                if expanded { "▾ " } else { "▸ " },
                Style::default().fg(if row.dimmed {
                    theme.accent_dim
                } else {
                    theme.accent
                }),
            ));
            // A project with nothing in it recedes rather than disappears: it
            // is still where you go to press `n`.
            let style = match row.kind {
                _ if row.dimmed => Style::default().fg(theme.dim),
                RowKind::Workspace(_) => Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
                // A group heads its agents the way a workspace used to head
                // everything: bold, so the sections read at a glance.
                RowKind::Group(..) => Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
                _ => Style::default().fg(theme.fg),
            };
            spans.push(Span::styled(row.label.clone(), style));
        }
        _ => spans.push(Span::raw(row.label.clone())),
    }

    if !row.note.is_empty() && !matches!(row.kind, RowKind::Agent(..)) {
        spans.push(Span::styled(
            format!("  {}", row.note),
            Style::default().fg(theme.dim),
        ));
    }
    Line::from(spans)
}

fn detail_lines(app: &App) -> Vec<Line<'static>> {
    let theme = app.theme;
    // Wrapping the detail pane's key/value rows once, since several arms use it.
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!(" {k:<9}"), Style::default().fg(theme.dim)),
            Span::styled(v, Style::default().fg(theme.fg)),
        ])
    };

    let Some(row) = app.selected_row() else {
        return vec![Line::from(Span::styled(
            " nothing selected",
            Style::default().fg(theme.dim),
        ))];
    };

    match row.kind {
        RowKind::Agent(w, p, e, a) => {
            let proj = &app.tree[w].projects[p];
            let env = &proj.envs[e];
            let name = row.label.clone();
            let status = row.status.clone().unwrap_or_default();
            let agent = match &env.agents {
                Load::Loaded(list) => list.get(a),
                _ => None,
            };

            let mut lines = vec![
                Line::from(Span::styled(
                    format!(" {name}"),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(vec![
                    Span::styled(
                        format!("  {} {}", status_glyph(&status), status),
                        Style::default().fg(status_color(theme, &status)),
                    ),
                    Span::styled(
                        format!("  ·  {}/{}", proj.name, env.name),
                        Style::default().fg(theme.dim),
                    ),
                ]),
                Line::from(""),
            ];

            // A card per session: what it is, and the last thing it said. The
            // last line is only knowable for a session we have a pane for —
            // the platform reports state, not output — so an unattached one
            // says how to get its output rather than pretending to have it.
            match agent.map(|agent| &agent.sessions) {
                Some(LoadSessions::Loaded(sessions)) => {
                    let live: Vec<_> = sessions
                        .iter()
                        .filter(|session| session.is_interesting())
                        .collect();
                    if live.is_empty() {
                        lines.push(Line::from(Span::styled(
                            "  no sessions running",
                            Style::default().fg(theme.dim),
                        )));
                        lines.push(Line::from(""));
                        lines.push(Line::from(Span::styled(
                            "  n starts one",
                            Style::default().fg(theme.dim),
                        )));
                    }
                    for session in live {
                        let connected = app
                            .sessions
                            .iter()
                            .find(|pane| pane.durable_name == session.name);
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {} ", if connected.is_some() { "▌" } else { " " }),
                                Style::default().fg(theme.accent),
                            ),
                            Span::styled(
                                session.name.clone(),
                                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("  {}", session_state(app, &session.name, session.running)),
                                Style::default().fg(theme.dim),
                            ),
                        ]));
                        let message = match connected.and_then(|pane| pane.last_line()) {
                            Some(line) => (truncate(&line, 60), theme.fg),
                            None => ("not connected — enter to attach".into(), theme.dim),
                        };
                        lines.push(Line::from(Span::styled(
                            format!("      {}", message.0),
                            Style::default().fg(message.1),
                        )));
                        lines.push(Line::from(""));
                    }
                }
                Some(LoadSessions::Loading) => lines.push(Line::from(Span::styled(
                    "  loading sessions…",
                    Style::default().fg(theme.dim),
                ))),
                Some(LoadSessions::Failed(err)) => lines.push(Line::from(Span::styled(
                    format!("  couldn't load sessions: {err}"),
                    Style::default().fg(theme.pending),
                ))),
                _ => lines.push(Line::from(Span::styled(
                    "  → to load its sessions",
                    Style::default().fg(theme.dim),
                ))),
            }
            lines
        }
        RowKind::Environment(w, p, e) | RowKind::Group(w, p, e) => {
            let proj = &app.tree[w].projects[p];
            let env = &proj.envs[e];
            let count = match &env.agents {
                super::app::Load::Loaded(l) => format!("{}", l.len()),
                super::app::Load::Loading => "loading…".into(),
                super::app::Load::Failed(_) => "unknown".into(),
                super::app::Load::NotLoaded => "→ to load".into(),
            };
            vec![
                Line::from(Span::styled(
                    format!(" {}/{}", proj.name, env.name),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                kv("agents", count),
                Line::from(""),
                Line::from(Span::styled(
                    " n creates one here · t targets it",
                    Style::default().fg(theme.dim),
                )),
            ]
        }
        RowKind::Project(w, p) => {
            let proj = &app.tree[w].projects[p];
            vec![
                Line::from(Span::styled(
                    format!(" {}", proj.name),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                kv("envs", proj.envs.len().to_string()),
                kv("id", proj.id.clone()),
            ]
        }
        RowKind::Workspace(w) => {
            let ws = &app.tree[w];
            vec![
                Line::from(Span::styled(
                    format!(" {}", ws.name),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                kv("projects", ws.projects.len().to_string()),
            ]
        }
        RowKind::Session(w, p, e, a, i) => {
            let mut lines = vec![Line::from(Span::styled(
                format!(" {}", row.label),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))];
            // The command lives here, not in the row: it is a whole launch
            // line, and this is the pane with room for it.
            if let Load::Loaded(agents) = &app.tree[w].projects[p].envs[e].agents
                && let Some(agent) = agents.get(a)
                && let LoadSessions::Loaded(sessions) = &agent.sessions
                && let Some(session) = sessions.get(i)
            {
                lines.push(Line::from(""));
                lines.push(kv(
                    "state",
                    session_state(app, &session.name, session.running).to_string(),
                ));
                lines.push(kv("kind", session.kind.to_lowercase()));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(" {}", session.command_summary()),
                    Style::default().fg(theme.fg),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " enter reattaches in the pane",
                Style::default().fg(theme.dim),
            )));
            lines
        }
        RowKind::OtherProjects => vec![
            Line::from(Span::styled(
                " projects without agents",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                " open one and press n to start an agent there",
                Style::default().fg(theme.dim),
            )),
        ],
        RowKind::Separator | RowKind::Note(..) | RowKind::Hint => vec![Line::from("")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::cloud_agent::tui::app::{
        Agent, EnvNode, Load, LoadSessions, ProjectNode, Screen, Target, WorkspaceNode,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    pub(super) fn app_with_tree() -> App {
        let tree = vec![WorkspaceNode {
            id: "ws_1".into(),
            name: "Railway".into(),
            expanded: true,
            projects: vec![ProjectNode {
                id: "proj_1".into(),
                name: "devtools".into(),
                expanded: true,
                envs: vec![EnvNode {
                    id: "env_prod".into(),
                    name: "production".into(),
                    expanded: true,
                    agents: Load::Loaded(vec![Agent {
                        id: "ca_1".into(),
                        name: "nimble-otter".into(),
                        status: "running".into(),
                        sessions: LoadSessions::NotLoaded,
                        expanded: false,
                    }]),
                }],
            }],
        }];
        App::new(
            tree,
            Some(Target {
                project_id: "proj_1".into(),
                project_name: "devtools".into(),
                environment_id: "env_prod".into(),
                environment_name: "production".into(),
            }),
            Some("claude"),
            None,
            None,
            true,
        )
    }

    pub(super) fn draw(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                render_with_layout(app, f);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every cell of a drawn frame, as `(symbol, background)`.
    fn cells(app: &App, w: u16, h: u16) -> Vec<Vec<(String, Color)>> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                render_with_layout(app, f);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| {
                        let cell = &buffer[(x, y)];
                        (cell.symbol().to_string(), cell.bg)
                    })
                    .collect()
            })
            .collect()
    }

    /// The rows a dialog covers, found by its border corners: the first row
    /// holding `╭` at or after `title`, through the matching `╰`.
    fn dialog_rows(grid: &[Vec<(String, Color)>], title: &str) -> (usize, usize, usize, usize) {
        let top = grid
            .iter()
            .position(|row| {
                let line: String = row.iter().map(|(s, _)| s.as_str()).collect();
                line.contains('╭') && line.contains(title)
            })
            .unwrap_or_else(|| panic!("no dialog titled {title}"));
        let left = grid[top].iter().position(|(s, _)| s == "╭").unwrap();
        let right = grid[top].iter().rposition(|(s, _)| s == "╮").unwrap();
        let bottom = (top + 1..grid.len())
            .find(|y| grid[*y][left].0 == "╰")
            .expect("a closed dialog");
        (top, bottom, left, right)
    }

    /// A dialog is a surface, not a window onto the screen behind it: every
    /// cell it covers carries its own background, so nothing underneath and no
    /// terminal wallpaper shows through.
    #[test]
    fn dialogs_are_filled_rather_than_transparent() {
        let mut app = app_with_tree();
        app.start_settings();
        let grid = cells(&app, 100, 40);
        let (top, bottom, left, right) = dialog_rows(&grid, "settings");
        let surface = app.theme.surface;
        for row in &grid[top..=bottom] {
            for (symbol, bg) in &row[left..=right] {
                // The key badges in the footer paint their own background;
                // everything else is the surface.
                assert!(
                    *bg == surface || *bg == app.theme.accent_dim,
                    "transparent cell {symbol:?} in the dialog: {bg:?}"
                );
            }
        }
    }

    /// The breathing room lives outside the boxes: a slim margin of untouched
    /// cells between the terminal's edges and everything the TUI draws, on
    /// every screen.
    #[test]
    fn the_page_keeps_clear_of_the_terminal_edges() {
        for screen in [Screen::Menu, Screen::Manage] {
            let mut app = app_with_tree();
            app.screen = screen;
            let grid = cells(&app, 100, 40);
            let blank = |cells: &[(String, Color)]| cells.iter().all(|(s, _)| s == " ");
            let h = grid.len();
            for y in 0..PAGE_MARGIN_Y as usize {
                assert!(blank(&grid[y]), "top margin row {y} has content");
                assert!(blank(&grid[h - 1 - y]), "bottom margin row has content");
            }
            for (y, row) in grid.iter().enumerate() {
                let w = row.len();
                assert!(
                    blank(&row[..PAGE_MARGIN_X as usize]),
                    "left margin has content at row {y}"
                );
                assert!(
                    blank(&row[w - PAGE_MARGIN_X as usize..]),
                    "right margin has content at row {y}"
                );
            }
        }
    }

    /// The last row the TUI actually draws on — the footer/key strip sits
    /// here, one page margin above the terminal's bottom edge.
    pub(super) fn last_drawn_line(out: &str) -> String {
        out.lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .to_string()
    }

    /// The menu prompt holds a paragraph's worth of writing room — six text
    /// rows inside the outline — and its breathing room sits outside the box:
    /// blank rows against the border, not padding within it.
    #[test]
    fn the_menu_prompt_is_tall_with_its_room_outside() {
        let app = app_with_tree();
        let out = draw(&app, 100, 40);
        let lines: Vec<&str> = out.lines().collect();
        let top = lines
            .iter()
            .position(|l| l.contains(" Prompt "))
            .expect("the prompt box");
        let bottom = (top + 1..lines.len())
            .find(|&y| lines[y].trim_start().starts_with("╰"))
            .expect("the prompt's bottom border");
        assert_eq!(bottom - top, 7, "six text rows inside the outline:\n{out}");
        for y in [top - 1, bottom + 1] {
            assert!(
                lines[y].trim().is_empty(),
                "row {y} should be the prompt's outside gap: {:?}",
                lines[y]
            );
        }
    }

    /// The wordmark must be full blocks and spaces only: box-drawing shadow
    /// glyphs render at a different weight in some monospace fonts and shear
    /// the whole thing.
    #[test]
    fn banner_uses_no_box_drawing_glyphs() {
        let stray: Vec<char> = BANNER
            .chars()
            .filter(|c| !matches!(c, '█' | ' ' | '\n'))
            .collect();
        assert!(
            stray.is_empty(),
            "non-block glyphs in the banner: {stray:?}"
        );
        assert_eq!(BANNER.lines().count(), BANNER_H as usize);

        // Every rendered row is padded to one width, or ratatui centres them
        // independently and the letters drift out of column.
        let widths: std::collections::HashSet<usize> = banner_lines(Theme::default_theme())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.chars().count()).sum())
            .collect();
        assert_eq!(
            widths,
            std::collections::HashSet::from([BANNER_W as usize]),
            "banner rows must all be {BANNER_W} wide, got {widths:?}"
        );
        // Nothing may exceed the declared width either — that would clip.
        assert!(
            BANNER
                .lines()
                .all(|l| l.chars().count() <= BANNER_W as usize),
            "a banner row is wider than BANNER_W"
        );

        // The Y's stem has to line up with the notch above it; an off-centre
        // join is what "not even" looks like.
        for line in BANNER.lines() {
            let padded = format!("{line:<width$}", width = BANNER_W as usize);
            let y: String = padded.chars().skip(47).collect();
            let mirrored: String = y.chars().rev().collect();
            assert_eq!(y, mirrored, "the Y must be symmetric: {y:?}");
        }
    }

    /// The menu's footer uses the same chord badges as the manage screen, so
    /// the two read as one product rather than two conventions.
    #[test]
    fn the_menu_footer_uses_chord_badges() {
        let app = app_with_tree();
        let out = draw(&app, 100, 40);
        let footer = out
            .lines()
            .rfind(|l| l.contains("launch"))
            .expect("the menu footer");
        assert!(footer.contains("enter"), "{footer}");
        assert!(footer.contains("settings"), "{footer}");
        assert!(
            !footer.contains("theme"),
            "the theme moved onto the settings card: {footer}"
        );
        // The target shortcut moved onto the target line itself — see
        // `target_shortcut_sits_on_its_own_line`.
        assert!(!footer.contains("target"), "{footer}");
        assert!(!footer.contains("menu"), "the arrow hint is gone: {footer}");
        // The old run-on line separated with interpuncts; the badges do not.
        assert!(!footer.contains(" · "), "{footer}");
    }

    /// The cards are a place to point at, not a list of commands: no key
    /// badges, and nothing that reads like one.
    #[test]
    fn the_menu_cards_carry_no_key_badges() {
        let app = app_with_tree();
        let out = draw(&app, 100, 40);
        let card = out
            .lines()
            .find(|l| l.contains("New Session"))
            .expect("the New Session card");
        assert!(!card.contains(" n "), "no key badge: {card}");

        let card = out
            .lines()
            .find(|l| l.contains("Manage Cloud Agents"))
            .expect("the Manage card");
        assert!(!card.contains(" m "), "no key badge: {card}");
    }

    /// Every card, including the first-run Setup one.
    const CARD_LINES: &[&str] = &[
        "New Session",
        "New Cloud Agent",
        "Manage Cloud Agents",
        "Setup",
    ];

    /// The cards sit under the middle of the prompt box, as a block — the names
    /// stay in one column rather than being centred line by line.
    #[test]
    fn the_menu_cards_are_centred_as_a_block() {
        let mut app = app_with_tree();
        app.configured = false;
        let width = 100u16;
        let out = draw(&app, width, 40);

        let starts: Vec<usize> = out
            .lines()
            .filter(|l| CARD_LINES.iter().any(|card| l.contains(card)))
            .map(|l| l.len() - l.trim_start().len())
            .collect();
        assert_eq!(starts.len(), 4, "three cards plus setup");
        assert!(
            starts.iter().all(|s| *s == starts[0]),
            "one left edge, not three: {starts:?}"
        );

        // And the block as a whole sits on the middle of the screen.
        let right_edge = out
            .lines()
            .filter(|l| CARD_LINES.iter().any(|card| l.contains(card)))
            .map(|l| l.trim_end().chars().count())
            .max()
            .expect("a card");
        let middle = (starts[0] + right_edge) / 2;
        assert!(
            middle.abs_diff(width as usize / 2) <= 2,
            "the block should be centred: {starts:?}..{right_edge} on {width}"
        );
    }

    /// Setup is on the menu only while there is nothing set up; after that
    /// the answers live behind the ⌥s in the footer, which is there either
    /// way.
    #[test]
    fn setup_is_a_card_only_on_a_first_run() {
        let mut app = app_with_tree();
        let out = draw(&app, 100, 40);
        assert!(!out.contains("Default agent, skills"), "{out}");
        assert!(
            out.contains("settings"),
            "the chord is still offered:\n{out}"
        );

        app.configured = false;
        let out = draw(&app, 100, 40);
        assert!(out.contains("Default agent, skills"), "{out}");
    }

    /// Where the prompt lands is its own line above the keys, not a chip inside
    /// the box being typed in.
    #[test]
    fn the_target_sits_above_the_shortcuts_not_in_the_prompt() {
        let app = app_with_tree();
        let out = draw(&app, 100, 40);
        let lines: Vec<&str> = out.lines().collect();

        let prompt_bottom = lines
            .iter()
            .position(|l| l.contains("claude") && l.contains("shift+tab"))
            .expect("the prompt box footer");
        assert!(
            !lines[prompt_bottom].contains("devtools"),
            "the target left the prompt box: {}",
            lines[prompt_bottom]
        );

        let target = lines
            .iter()
            .position(|l| l.contains("Target Project"))
            .expect("the target indicator");
        assert!(
            lines[target].contains("devtools (production)"),
            "{}",
            lines[target]
        );

        let footer = lines
            .iter()
            .position(|l| l.contains("launch"))
            .expect("the footer");
        assert!(target < footer, "the target sits above the shortcuts");
    }

    /// The shortcut for changing the target sits right on the field it acts
    /// on, not lumped into the footer's own chord list where it read like a
    /// command with no object.
    #[test]
    fn the_target_shortcut_sits_on_its_own_line() {
        let app = app_with_tree();
        let out = draw(&app, 100, 40);
        let target = out
            .lines()
            .find(|l| l.contains("Target Project"))
            .expect("the target indicator");
        let chord_at = target.find("^t").expect("the chord badge: {target}");
        let label_at = target.find("Target Project").unwrap();
        assert!(
            chord_at < label_at,
            "the chord badge comes before the label: {target}"
        );

        let footer = out
            .lines()
            .rfind(|l| l.contains("settings"))
            .expect("the menu footer");
        assert!(
            !footer.contains("^t"),
            "the chord moved out of the footer: {footer}"
        );
    }

    /// With nowhere to launch, the indicator says so rather than going blank.
    #[test]
    fn no_target_says_not_set() {
        let mut app = app_with_tree();
        app.target = None;
        let out = draw(&app, 100, 40);
        assert!(out.contains("Target Project  not set"), "{out}");
    }

    /// The clickable boxes have to be where the cards actually drew, or a click
    /// lands on the wrong one — the only way to know is to read them out of a
    /// real frame.
    #[test]
    fn the_recorded_card_boxes_match_the_drawn_rows() {
        use crate::commands::cloud_agent::tui::app::PaneRects;

        let mut app = app_with_tree();
        app.configured = false; // all four cards
        let mut terminal = Terminal::new(TestBackend::new(100, 44)).unwrap();
        let mut rects = PaneRects::default();
        terminal
            .draw(|f| {
                let (r, _) = render_with_layout(&app, f);
                rects = r;
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let out = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let lines: Vec<&str> = out.lines().collect();

        for (i, (label, _)) in app.cards().iter().enumerate() {
            let drawn = lines
                .iter()
                .position(|l| l.contains(label))
                .unwrap_or_else(|| panic!("{label} was not drawn"));
            let box_ = rects.cards[i];
            assert_eq!(
                box_.y as usize, drawn,
                "{label}: recorded at {}, drawn at {drawn}",
                box_.y
            );
            assert!(box_.h >= 1, "{label} has no clickable height");
            // A wrapped description is part of the same card.
            let wrapped = out
                .lines()
                .nth(drawn + 1)
                .is_some_and(|l| !l.trim().is_empty() && !CARD_LINES.iter().any(|c| l.contains(c)));
            assert_eq!(
                box_.h >= 2,
                wrapped,
                "{label}: height {} does not match its wrapping",
                box_.h
            );
        }

        // And the prompt box is where it was drawn.
        let prompt = lines
            .iter()
            .position(|l| l.contains("╭ Prompt"))
            .expect("the prompt box");
        assert_eq!(rects.prompt.y as usize, prompt);
    }

    /// The pane renders history from any depth, not just the last screenful.
    /// This drives the real draw path — `render_session` → `screen_lines` →
    /// `Screen::cell` — with the view sitting several screens back, which the
    /// old emulator could not compose at all.
    #[test]
    fn a_deeply_scrolled_pane_draws_old_history() {
        use crate::commands::cloud_agent::tui::session::Session;

        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.attach_session(
            Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );

        let session = app.sessions.last_mut().expect("just attached");
        session.resize(6, 40);
        for i in 0..80 {
            session.send(format!("line-{i}\r\n").as_bytes());
        }
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let seen = session
                .with_screen(|screen| screen.contents().contains("line-79"))
                .unwrap_or(false);
            if seen {
                break;
            }
        }
        session.scroll_by(isize::MAX);
        assert!(session.scrolled_back());

        let out = draw(&app, 92, 20);
        assert!(
            out.contains("line-0"),
            "the top of history should be on screen:\n{out}"
        );
        assert!(
            !out.contains("line-79"),
            "the tail should be scrolled out of view:\n{out}"
        );
        assert!(
            out.contains("scrolled back"),
            "the pane should say where it is:\n{out}"
        );
    }

    /// A drag that reached the clipboard says so in the corner — the only other
    /// evidence is the clipboard itself, which is not on the screen.
    /// The PTY and the drawn pane are the same size. An emulator wrapping
    /// wider than its pane puts the tail of every row somewhere the screen
    /// never shows — four characters per fold of anything long enough to
    /// wrap, which sheared OAuth login URLs into invalid links.
    #[test]
    fn the_session_pty_matches_the_drawn_pane() {
        use crate::commands::cloud_agent::tui::session::Session;

        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.attach_session(
            Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        let mut rects = crate::commands::cloud_agent::tui::app::PaneRects::default();
        terminal
            .draw(|f| {
                let (r, _) = render_with_layout(&app, f);
                rects = r;
            })
            .unwrap();
        let (rows, cols) =
            session_pane_size(Some(ratatui::layout::Size::new(100, 40)), false).unwrap();
        assert_eq!(
            (cols, rows),
            (rects.session.w, rects.session.h),
            "the PTY must be exactly the pane the emulator is drawn into"
        );
    }

    #[test]
    fn a_toast_floats_in_the_bottom_corner() {
        use crate::commands::cloud_agent::tui::session::Session;

        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.attach_session(
            Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        app.toast("Copied 3 lines");
        let out = draw(&app, 92, 20);
        let lines: Vec<&str> = out.lines().collect();

        let row = lines
            .iter()
            .position(|l| l.contains("Copied 3 lines"))
            .expect("the toast");
        assert!(out.contains("✓"), "{out}");

        // Bottom right: below the middle, right of it, and clear of both the
        // key strip on the last row and the pane border above it.
        assert!(row > lines.len() / 2, "in the bottom half: {row}");
        let start = lines[row]
            .chars()
            .collect::<Vec<_>>()
            .windows(6)
            .position(|w| w.iter().collect::<String>() == "Copied")
            .expect("the toast text");
        assert!(start > 92 / 2, "on the right: {start}");
        let strip = last_drawn_line(&out);
        assert!(
            strip.contains("keys"),
            "the key strip is untouched: {strip}"
        );
        // The toast is a closed box: its bottom border sits under the text
        // (with the padding between them) and above the key strip.
        let closed = (row + 1..lines.len())
            .find(|y| lines[*y].contains("╰"))
            .expect("the toast's bottom border");
        assert!(
            closed < lines.len() - 1,
            "the toast should close above the key strip: {}",
            lines[closed]
        );
        assert!(
            lines[row + 1..=closed]
                .iter()
                .all(|l| !l.contains("Copied")),
            "the text is inside the box only once"
        );
    }

    /// A failure must not wear a tick.
    #[test]
    fn a_failed_copy_is_marked_as_one() {
        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.toast_error("Couldn't copy: no clipboard");
        let out = draw(&app, 92, 20);
        assert!(out.contains("✕"), "{out}");
        assert!(!out.contains("✓"), "{out}");
    }

    /// And it leaves on its own rather than sitting there.
    #[test]
    fn an_expired_toast_is_not_drawn() {
        use crate::commands::cloud_agent::tui::app::{TOAST_LIFETIME, Toast};

        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.toast = Some(Toast {
            text: "Copied 3 lines".into(),
            at: std::time::Instant::now() - TOAST_LIFETIME,
            ok: true,
        });
        let out = draw(&app, 92, 20);
        assert!(!out.contains("Copied 3 lines"), "{out}");
    }

    /// The way out of a focused session is a key, and the key strip already has
    /// it — the pane border does not need to say it twice.
    #[test]
    fn a_focused_pane_does_not_repeat_the_escape_chord() {
        use crate::commands::cloud_agent::tui::session::Session;

        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.attach_session(
            Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        let out = draw(&app, 92, 20);
        let border = out
            .lines()
            .find(|l| l.trim_start().starts_with("╰"))
            .expect("the pane's bottom border");
        assert!(!border.contains("to leave"), "{border}");
        assert!(
            last_drawn_line(&out).contains("stop typing"),
            "the key strip still has it:\n{out}"
        );
    }

    /// Maximized, the tree is gone and the session has the width.
    #[test]
    fn a_maximized_session_takes_the_whole_screen() {
        use crate::commands::cloud_agent::tui::session::Session;

        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.attach_session(
            Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        let before = draw(&app, 100, 30);
        assert!(before.contains("cloud agents"), "the tree is there first");

        app.maximized = true;
        let out = draw(&app, 100, 30);
        assert!(!out.contains(" cloud agents "), "the tree is gone:\n{out}");
        assert!(!out.contains("devtools"), "no tree rows:\n{out}");
        assert!(out.contains("restore the tree"), "the way back:\n{out}");

        // The session pane spans the width rather than starting at the old
        // tree boundary.
        let pane = out
            .lines()
            .find(|l| l.contains("╭"))
            .expect("the session pane");
        assert_eq!(
            pane.chars().position(|c| c == '╭'),
            Some(PAGE_MARGIN_X as usize),
            "the pane starts at the page's left edge: {pane}"
        );
    }

    /// The emulator is sized to whichever pane it is drawn into, or a maximized
    /// session would wrap where the tree used to be.
    #[test]
    fn the_emulator_follows_the_maximized_pane() {
        let size = Some(ratatui::layout::Size {
            width: 100,
            height: 30,
        });
        let (_, split) = session_pane_size(size, false).unwrap();
        let (_, full) = session_pane_size(size, true).unwrap();
        // Inside the page margin, like the panes it must agree with.
        assert_eq!(split, 100 - PAGE_MARGIN_X * 2 - TREE_W - 2);
        assert_eq!(full, 100 - PAGE_MARGIN_X * 2 - 2);

        // And a terminal too narrow for two panes is wide enough for one.
        let narrow = Some(ratatui::layout::Size {
            width: 50,
            height: 20,
        });
        assert!(session_pane_size(narrow, false).is_none());
        assert!(session_pane_size(narrow, true).is_some());
    }

    /// Choosing which agent a new session goes on is the same card, so the two
    /// questions look like one flow.
    #[test]
    fn the_agent_picker_lists_the_agents_with_their_status() {
        use crate::commands::cloud_agent::tui::app::AgentPicker;

        let mut app = app_with_tree();
        app.agent_pick = Some(AgentPicker {
            options: vec![
                ("ca_1".into(), "nimble-otter".into(), "running".into()),
                ("ca_2".into(), "brisk-heron".into(), "sleeping".into()),
            ],
            cursor: 0,
        });
        app.screen = Screen::AgentPick;
        let out = draw(&app, 100, 34);
        assert!(out.contains("Which cloud agent?"), "{out}");
        assert!(out.contains("nimble-otter"), "{out}");
        assert!(out.contains("sleeping"), "the status rides along: {out}");
        assert!(out.contains("new session"), "{out}");
    }

    /// The descriptions are longer than the prompt box is wide, so they wrap
    /// into the column beside the name — the block never grows past the box.
    #[test]
    fn card_descriptions_wrap_inside_the_prompt_box() {
        let app = app_with_tree();
        for width in [120u16, 100, 90, 80, 60] {
            let out = draw(&app, width, 44);
            let lines: Vec<&str> = out.lines().collect();

            let box_left = lines
                .iter()
                .find(|l| l.contains("╭ Prompt"))
                .map(|l| l.chars().position(|c| c == '╭').unwrap())
                .expect("the prompt box");
            let box_right = lines
                .iter()
                .find(|l| l.contains("╭ Prompt"))
                .map(|l| l.chars().position(|c| c == '╮').unwrap())
                .expect("the prompt box");

            // Every card line, continuations included, lives inside the box.
            for line in lines
                .iter()
                .filter(|l| CARD_LINES.iter().any(|card| l.contains(card)) || l.contains("project"))
            {
                let start = line.len() - line.trim_start().len();
                let end = line.trim_end().chars().count();
                assert!(
                    start >= box_left && end <= box_right + 1,
                    "at {width}: {start}..{end} outside {box_left}..{box_right}\n{line}"
                );
            }

            // And the sentence still arrives in full, across however many lines
            // it took.
            let text: String = lines
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                text.contains("Create a new session on a Cloud Agent in your default project"),
                "at {width}: the description was lost\n{out}"
            );
        }
    }

    /// Below the width where even a wrapped sentence would be shredded, the
    /// names stand alone.
    #[test]
    fn very_narrow_cards_keep_their_names_and_lose_the_descriptions() {
        let app = app_with_tree();
        let out = draw(&app, 46, 40);
        assert!(out.contains("New Session"), "{out}");
        assert!(out.contains("Manage Cloud Agents"), "{out}");
        assert!(!out.contains("Create a new"), "{out}");
        assert!(
            out.lines().all(|l| l.trim_end().chars().count() <= 46),
            "nothing runs off the edge:\n{out}"
        );
    }

    #[test]
    fn wrap_words_breaks_on_spaces_and_keeps_every_word() {
        let wrapped = wrap_words("Create a new Cloud Agent in your default project", 20);
        assert!(wrapped.len() > 1, "{wrapped:?}");
        assert!(
            wrapped.iter().all(|l| l.chars().count() <= 20),
            "{wrapped:?}"
        );
        assert_eq!(
            wrapped.join(" "),
            "Create a new Cloud Agent in your default project"
        );
        assert_eq!(wrap_words("", 20), Vec::<String>::new());
    }

    /// Below the banner threshold the screen still has to be usable — a
    /// terminal that small is common inside a split pane.
    #[test]
    fn menu_degrades_to_a_wordmark_when_small() {
        let app = app_with_tree();
        let out = draw(&app, 50, 20);
        assert!(!out.contains("█"), "banner should be dropped:\n{out}");
        assert!(out.contains("RAILWAY CLOUD-AGENTS"));
        assert!(out.contains("Prompt"));
    }

    #[test]
    fn manage_renders_the_tree_and_the_detail_pane() {
        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.cursor = app
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        let out = draw(&app, 100, 30);
        // The group header carries the project; the environment shows in the
        // detail pane rather than as a level of its own.
        assert!(out.contains("devtools"));
        assert!(out.contains("production"));
        assert!(out.contains("nimble-otter"));
        assert!(
            out.contains("running"),
            "status belongs in the detail pane:\n{out}"
        );
        assert!(
            out.contains("connect"),
            "the footer names the action:\n{out}"
        );
    }

    /// One pane below 70 columns: two would leave the tree unreadable.
    #[test]
    fn manage_drops_the_detail_pane_when_narrow() {
        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        let out = draw(&app, 60, 20);
        assert!(out.contains("nimble-otter"));
        // The pane's own title, not any line that happens to say "agent" —
        // the hint line mentions one.
        assert!(
            !out.contains("╭ agent "),
            "detail pane should be gone:\n{out}"
        );
    }

    /// The target chooser is the setup flow's card, over the menu — one list of
    /// places to run, not a trip through the management tree.
    #[test]
    fn the_target_picker_is_a_card_over_the_menu() {
        let mut app = app_with_tree();
        app.start_target_pick();
        let out = draw(&app, 100, 34);
        assert!(out.contains("target"), "{out}");
        assert!(out.contains("Where should Cloud Agents run?"), "{out}");
        assert!(out.contains("devtools (production)"), "{out}");
        assert!(out.contains("set target"), "{out}");
        assert!(
            !out.contains("╭ agents"),
            "the management tree must not be behind it:\n{out}"
        );
    }

    /// An open session takes over the right pane, and the agent row says how
    /// many are running on it.
    #[test]
    fn manage_shows_an_open_session_in_the_pane() {
        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.attach_session(
            crate::commands::cloud_agent::tui::session::Session::for_test("ca_1", "nimble-otter")
                .unwrap(),
            "ca_1".into(),
        );
        let out = draw(&app, 100, 30);
        // The pane is titled by the agent and the session it is attached to.
        assert!(out.contains("nimble-otter"), "{out}");
        assert!(
            out.contains("test"),
            "the durable name in the title:\n{out}"
        );
        // And the old bottom-left list is gone for good.
        assert!(!out.contains("sessions ·"), "{out}");
    }

    /// While a launch runs, the wait belongs in the pane the session will
    /// appear in — with the tree still beside it.
    #[test]
    fn the_loading_state_renders_in_the_session_pane() {
        let mut app = app_with_tree();
        app.start_loading(&crate::commands::cloud_agent::tui::LaunchRequest {
            project_id: "proj_1".into(),
            environment_id: "env_prod".into(),
            agent_id: None,
            session_name: None,
            force_new: false,
            new_session: false,
            harness: "claude".into(),
            prompt: Some("fix the failing tests".into()),
            label: "devtools/production".into(),
        });
        app.loading_step("Creating a cloud agent".into());

        let out = draw(&app, 100, 30);
        assert!(out.contains("starting"), "pane title:\n{out}");
        assert!(out.contains("fix the failing tests"), "the task:\n{out}");
        assert!(out.contains("Creating a cloud agent"), "steps:\n{out}");
        // The tree is still there.
        assert!(out.contains("devtools"), "tree stays visible:\n{out}");

        // The block of steps is centred in its pane, not pinned to a border.
        // Measured between the borders either side of the text, since the tree
        // draws its own on the same rows — and entirely in `char` units: the
        // line is full of multi-byte box glyphs, so byte offsets would land
        // mid-character and the arithmetic would be quietly wrong.
        let line: Vec<char> = out
            .lines()
            .find(|l| l.contains("Creating a cloud agent"))
            .unwrap()
            .chars()
            .collect();
        let needle: Vec<char> = "Creating a cloud agent".chars().collect();
        let text_start = line
            .windows(needle.len())
            .position(|w| w == needle.as_slice())
            .expect("the step text");
        let text_end = text_start + needle.len() - 1;
        // Include the step's marker, which is part of the block being centred.
        let block_start = text_start.saturating_sub(2);
        let left_border = (0..block_start)
            .rev()
            .find(|i| line[*i] == '│')
            .expect("a border to the left");
        let right_border = (text_end + 1..line.len())
            .find(|i| line[*i] == '│')
            .expect("a border to the right");
        let gap_left = block_start - left_border - 1;
        let gap_right = right_border - text_end - 1;
        assert!(gap_left > 2, "hugging the left border: {gap_left}");
        assert!(
            gap_left.abs_diff(gap_right) <= 4,
            "left {gap_left} and right {gap_right} gaps should be close:\n{out}"
        );
    }

    /// The footer carries the actions that apply where the cursor is, with help
    /// pinned right; everything else is behind `?`.
    #[test]
    fn the_footer_shows_the_actions_for_the_selected_row() {
        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.cursor = app
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();

        let out = draw(&app, 120, 30);
        let footer = last_drawn_line(&out);
        let footer = footer.as_str();
        assert!(footer.contains("connect"), "{footer}");
        assert!(footer.contains("new session"), "{footer}");
        assert!(footer.contains("delete agent"), "{footer}");
        // The agent is running, so it offers sleep and not wake.
        assert!(footer.contains("sleep"), "{footer}");
        assert!(!footer.contains("wake"), "{footer}");
        // Help is pinned to the right edge.
        assert!(footer.trim_end().ends_with("keys"), "{footer}");
        assert!(
            footer.find("keys").unwrap() > footer.find("connect").unwrap(),
            "help should be right of the actions:\n{footer}"
        );
    }

    /// A sleeping agent offers wake instead — never both, since only one of
    /// them does anything.
    #[test]
    fn the_footer_offers_wake_for_a_sleeping_agent() {
        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        if let Load::Loaded(agents) = &mut app.tree[0].projects[0].envs[0].agents {
            agents[0].status = "sleeping".into();
        }
        app.cursor = app
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();

        let footer = last_drawn_line(&draw(&app, 120, 30));
        assert!(footer.contains("wake"), "{footer}");
        assert!(!footer.contains("sleep"), "{footer}");
    }

    /// On a project there is nothing to sleep or delete, so it says less.
    #[test]
    fn the_footer_is_shorter_on_a_project() {
        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.cursor = app
            .rows()
            .iter()
            .position(|r| r.label == "devtools")
            .unwrap();
        let footer = last_drawn_line(&draw(&app, 120, 30));
        assert!(footer.contains("new agent"), "{footer}");
        assert!(!footer.contains("delete"), "{footer}");
        assert!(footer.trim_end().ends_with("keys"), "{footer}");
    }

    /// `?` still carries everything the footer leaves out.
    #[test]
    fn the_overlay_has_the_rest() {
        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        app.keys_open = true;
        // Two rows taller than before: the page margin trims the overlay's
        // room, and this list is exactly long enough to notice.
        let out = draw(&app, 100, 34);
        assert!(out.contains("keys"));
        assert!(out.contains("refresh"), "{out}");
        assert!(out.contains("⌥/⇧esc / ^]"), "{out}");
        assert!(out.contains("any key closes"));
    }

    /// Standing on an agent shows its cards, even while one of its sessions is
    /// open — the pane follows the selection, not merely what is running.
    #[test]
    fn an_agent_shows_its_cards_while_a_session_runs() {
        let mut app = app_with_tree();
        app.screen = Screen::Manage;
        if let Load::Loaded(agents) = &mut app.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![
                crate::commands::cloud_agent::tui::app::ConsoleSession {
                    name: "claude-one".into(),
                    kind: "SHELL".into(),
                    command: None,
                    running: true,
                    attached: true,
                },
            ]);
        }
        let mut pane =
            crate::commands::cloud_agent::tui::session::Session::for_test("ca_1", "nimble-otter")
                .unwrap();
        pane.durable_name = "claude-one".into();
        app.sessions = vec![pane];
        app.active = Some(0);
        app.focus = ManageFocus::Tree;
        app.cursor = app
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();

        let out = draw(&app, 110, 30);
        assert!(
            out.contains("claude-one"),
            "the card names the session:\n{out}"
        );
        assert!(
            out.contains("running") || out.contains("attached"),
            "the card carries its state:\n{out}"
        );

        // Move onto the session itself and the pane takes over.
        app.cursor = app
            .rows()
            .iter()
            .position(|r| r.label == "claude-one")
            .unwrap();
        let out = draw(&app, 110, 30);
        assert!(
            out.contains("nimble-otter · claude-one"),
            "the session pane's title:\n{out}"
        );
    }

    /// Typing past the bottom of the prompt scrolls it, rather than quietly
    /// hiding what is being typed.
    #[test]
    fn a_long_prompt_scrolls_to_the_cursor() {
        let mut app = app_with_tree();
        // Far more than the box can show at once.
        app.prompt = "fix the failing retry tests in the worker service and then \
             update the changelog and open a pull request describing what changed"
            .repeat(3);
        let out = draw(&app, 100, 40);
        // The tail is what matters: the end of the draft has to be on screen.
        // The last words rather than a whole phrase — the box wraps, and where
        // a line breaks is the box's business.
        assert!(
            out.contains("what changed"),
            "the end of the prompt should be visible:\n{out}"
        );
    }

    /// Wrapping arithmetic the scroll depends on.
    #[test]
    fn wrapped_lines_counts_rows() {
        assert_eq!(wrapped_lines("", 10), 1);
        assert_eq!(wrapped_lines("short", 10), 1);
        assert_eq!(wrapped_lines("one two three", 8), 2);
        // A word longer than the box hard-wraps rather than vanishing.
        assert!(wrapped_lines(&"x".repeat(25), 10) >= 3);
        assert_eq!(wrapped_lines("anything", 0), 1, "no divide by zero");
    }

    /// The task box is a fixed third of the pane, so a long task cannot drag
    /// the panel open and shove the steps to the margin.
    #[test]
    fn a_long_task_does_not_widen_the_loading_panel() {
        let mut app = app_with_tree();
        app.start_loading(&crate::commands::cloud_agent::tui::LaunchRequest {
            project_id: "proj_1".into(),
            environment_id: "env_prod".into(),
            agent_id: None,
            session_name: None,
            force_new: false,
            new_session: false,
            harness: "claude".into(),
            prompt: Some(
                "fix the failing retry tests in the worker service and update the changelog".into(),
            ),
            label: "devtools/production".into(),
        });
        app.loading_step("Creating a cloud agent".into());

        let out = draw(&app, 120, 30);
        // In char units throughout: the row is full of multi-byte glyphs, so a
        // byte offset from `find` would land mid-character and the arithmetic
        // would be quietly wrong.
        let chars: Vec<char> = out
            .lines()
            .find(|l| l.contains("Creating a cloud agent"))
            .unwrap()
            .chars()
            .collect();
        let needle: Vec<char> = "Creating a cloud agent".chars().collect();
        let text_at = chars
            .windows(needle.len())
            .position(|w| w == needle.as_slice())
            .expect("the step text");
        // The spinner marker is part of the block being centred.
        let block_start = text_at.saturating_sub(2);
        let left = (0..block_start).rev().find(|i| chars[*i] == '│').unwrap();
        let right = (text_at + needle.len()..chars.len())
            .find(|i| chars[*i] == '│')
            .unwrap();
        let gap_left = block_start - left - 1;
        let gap_right = right - (text_at + needle.len());
        assert!(
            gap_left.abs_diff(gap_right) <= 6,
            "the steps should stay centred: left {gap_left}, right {gap_right}\n{out}"
        );
    }

    /// A list of names is a list of names: no blank row under each one.
    #[test]
    fn wizard_rows_without_a_description_have_no_gap() {
        let mut app = app_with_tree();
        // A second environment, so "adjacent" means something.
        app.tree[0].projects[0].envs.push(EnvNode {
            id: "env_stg".into(),
            name: "staging".into(),
            expanded: false,
            agents: Load::NotLoaded,
        });
        app.skills_source = None;
        app.start_wizard(false);
        if let Some(w) = app.wizard.as_mut() {
            w.step = crate::commands::cloud_agent::tui::wizard::Step::Target;
            // Expand the workspace's only project to reveal its environments
            // — leaf rows carry no description.
            w.workspaces[0].projects[0].expanded = true;
        }

        let out = draw(&app, 100, 30);
        let lines: Vec<&str> = out.lines().collect();
        let first = lines
            .iter()
            .position(|l| l.contains("production"))
            .expect("the environment row");
        assert!(
            lines[first + 1].contains("staging"),
            "rows should be adjacent:\n{out}"
        );
    }

    /// The settings card shows every value beside its name, and the
    /// highlighted one wears the cycle arrows.
    #[test]
    fn the_settings_card_shows_values_in_place() {
        let mut app = app_with_tree();
        app.skills_source = Some("claude".into());
        app.skills_enabled = true;
        app.start_settings();

        let out = draw(&app, 100, 40);
        assert!(out.contains("Cloud agent settings"), "{out}");
        assert!(
            out.contains("‹ claude ›"),
            "the highlighted row cycles in place:\n{out}"
        );
        assert!(out.contains("on · claude"), "{out}");
        assert!(out.contains("Railway"), "the theme's label:\n{out}");
        assert!(out.contains("Run first-time setup again"), "{out}");
        assert!(
            out.contains("not set"),
            "no default project reads as such:\n{out}"
        );
    }

    /// The project row opens the wizard's question as a sub-card and comes
    /// straight back, rather than walking the rest of a flow.
    #[test]
    fn the_settings_project_picker_is_the_setup_question() {
        let mut app = app_with_tree();
        app.start_settings();
        if let Some(settings) = app.settings.as_mut() {
            settings.down(); // the project row
            settings.select(); // opens the picker
        }

        let out = draw(&app, 100, 40);
        assert!(out.contains("Where should agents live?"), "{out}");
        assert!(out.contains("devtools (production)"), "{out}");
        assert!(out.contains("Decide later"), "{out}");
    }

    /// A tree with nothing in it must not panic the renderer.
    #[test]
    fn manage_survives_an_empty_tree() {
        let mut app = App::new(Vec::new(), None, None, None, None, true);
        app.screen = Screen::Manage;
        let out = draw(&app, 80, 24);
        assert!(out.contains("RAILWAY CLOUD-AGENTS"));
    }

    /// The gate card: name and fingerprint each on their own line (together
    /// they outrun the card and wrap mid-fingerprint), and the answers in the
    /// same badge-and-label chords as the footers.
    #[test]
    fn the_ssh_gate_card_lays_out_key_and_answers() {
        use crate::commands::cloud_agent::tui::app::{SshGate, SshKeyOffer};
        let mut app = app_with_tree();
        app.ssh_gate = Some(SshGate {
            offer: SshKeyOffer {
                name: "raildesk-deploy".into(),
                fingerprint: "SHA256:hlDEs7CV5clc1lMfsMxr/CPeuKuJNn9hJxjsy1e9zLc".into(),
                public_key: "ssh-ed25519 AAAA test".into(),
            },
            then: None,
        });
        let out = draw(&app, 80, 30);
        assert!(out.contains("Register your SSH key with Railway?"), "{out}");
        assert!(out.contains(" y "), "{out}");
        assert!(out.contains("Yes — register this key"), "{out}");
        assert!(out.contains("No, not now"), "{out}");

        let name_line = out
            .lines()
            .position(|l| l.contains("raildesk-deploy"))
            .expect("key name shown");
        let fp_line = out
            .lines()
            .position(|l| l.contains("SHA256:hlDEs7CV5clc1lMfsMxr/CPeuKuJNn9hJxjsy1e9zLc"))
            .expect("full fingerprint shown, unwrapped");
        assert_eq!(fp_line, name_line + 1, "fingerprint sits under the name");

        // The answers are centered under the copy, not left-aligned with it.
        let col = |needle: &str| {
            out.lines()
                .find_map(|l| l.find(needle))
                .unwrap_or_else(|| panic!("{needle} not shown"))
        };
        assert!(
            col("Yes — register this key") > col("raildesk-deploy"),
            "the answers should sit centered, right of the left-aligned copy"
        );
    }
}
