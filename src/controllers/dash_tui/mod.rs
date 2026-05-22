use std::io::stdout;
use std::panic;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

#[derive(Clone, Debug)]
pub struct DashTuiParams {
    pub project: Option<String>,
    pub environment: Option<String>,
}

#[derive(Clone, Debug)]
struct DashApp {
    params: DashTuiParams,
}

impl DashApp {
    fn new(params: DashTuiParams) -> Self {
        Self { params }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;

    let rollback = scopeguard::guard((), |_| {
        restore_terminal();
    });

    execute!(stdout(), EnterAlternateScreen, Hide, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    std::mem::forget(rollback);

    Ok(terminal)
}

fn restore_terminal() {
    let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

pub async fn run(params: DashTuiParams) -> Result<()> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let _cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    let app = DashApp::new(params);
    let mut events = EventStream::new();

    loop {
        terminal.draw(|frame| render(frame, &app))?;

        tokio::select! {
            Some(Ok(event)) = events.next() => {
                if should_quit(event, &mut terminal) {
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }

    Ok(())
}

fn should_quit(event: Event, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> bool {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            matches!(key.code, KeyCode::Char('q'))
                || (matches!(key.code, KeyCode::Char('c'))
                    && key.modifiers.contains(KeyModifiers::CONTROL))
        }
        Event::Resize(_, _) => {
            let _ = terminal.clear();
            false
        }
        _ => false,
    }
}

fn render(frame: &mut Frame<'_>, app: &DashApp) {
    let area = frame.area();
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(3),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Railway Dashboard ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("phase 1 placeholder"),
        ]))
        .block(Block::default().borders(Borders::ALL).title("railway dash")),
        header,
    );

    render_placeholder(frame, body, app);

    frame.render_widget(
        Paragraph::new("q quit • Ctrl-C quit • project cards and service views coming next")
            .block(Block::default().borders(Borders::ALL).title("controls"))
            .wrap(Wrap { trim: true }),
        footer,
    );
}

fn render_placeholder(frame: &mut Frame<'_>, area: Rect, app: &DashApp) {
    frame.render_widget(Clear, area);

    let requested_project = app.params.project.as_deref().unwrap_or("(linked project)");
    let requested_environment = app
        .params
        .environment
        .as_deref()
        .unwrap_or("(linked/default environment)");

    let content = vec![
        Line::from(Span::styled(
            "Dashboard shell is wired up.",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from("This is the initial TUI scaffold for `railway dash`."),
        Line::from(
            "It validates auth before opening the alternate screen and restores the terminal on exit.",
        ),
        Line::default(),
        Line::from(vec![
            Span::styled(
                "requested project: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(requested_project),
        ]),
        Line::from(vec![
            Span::styled(
                "requested environment: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(requested_environment),
        ]),
        Line::default(),
        Line::from("Planned next steps:"),
        Line::from("• project cards / linked-project entry"),
        Line::from("• project overview with service cards"),
        Line::from("• handoff into existing metrics and volume-browser TUIs"),
        Line::from("• logs and deployment flows"),
    ];

    frame.render_widget(
        Paragraph::new(content)
            .block(Block::default().borders(Borders::ALL).title("placeholder"))
            .wrap(Wrap { trim: true }),
        area,
    );
}
