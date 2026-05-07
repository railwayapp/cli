mod app;
mod ui;

use std::{collections::HashMap, io::stdout, panic};

pub use app::{RegionRow, ScaleTuiAction, ScaleTuiApp, ScaleTuiFocus, ScaleTuiMode};

use anyhow::Result;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use serde_json::Value;

use crate::gql::queries;

pub enum ScaleTuiOutput {
    Apply(HashMap<String, u64>),
    Cancelled,
}

pub struct ScaleTuiParams {
    pub service_name: String,
    pub environment_name: String,
    pub regions: queries::regions::ResponseData,
    pub existing: Value,
}

pub fn run(params: ScaleTuiParams) -> Result<ScaleTuiOutput> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let _cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    let mut app = ScaleTuiApp::new(
        params.service_name,
        params.environment_name,
        params.regions,
        &params.existing,
    );

    loop {
        terminal.draw(|frame| ui::render(&app, frame))?;

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match app.handle_key(key) {
                ScaleTuiAction::Continue => {}
                ScaleTuiAction::Apply(changes) => return Ok(ScaleTuiOutput::Apply(changes)),
                ScaleTuiAction::Cancel => return Ok(ScaleTuiOutput::Cancelled),
            },
            Event::Resize(_, _) => terminal.clear()?,
            _ => {}
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal() {
    let _ = execute!(stdout(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}
