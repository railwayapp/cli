use indicatif::{ProgressBar, ProgressStyle};
use tokio::time::Duration;

use crate::consts::TICK_STRING;

/// Single-line "rotating cube" frames: a white square whose lit quadrant spins
/// clockwise, reading like a tumbling cube face.
const CUBE_TICK_STRING: &str = "◰◳◲◱◼";

/// A spinner that animates a small rotating cube. Use for create/destroy waits.
pub fn create_cube_spinner(message: String) -> ProgressBar {
    let spinner = ProgressBar::new_spinner()
        .with_style(
            ProgressStyle::default_spinner()
                .tick_chars(CUBE_TICK_STRING)
                .template("{spinner:.cyan} {msg}")
                .expect("Failed to create cube spinner template"),
        )
        .with_message(message);

    spinner.enable_steady_tick(Duration::from_millis(120));
    spinner
}

pub fn create_spinner(message: String) -> ProgressBar {
    let spinner = ProgressBar::new_spinner()
        .with_style(
            ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")
                .expect("Failed to create spinner template"),
        )
        .with_message(message);

    spinner.enable_steady_tick(Duration::from_millis(100));
    spinner
}

pub fn create_spinner_if(show: bool, message: String) -> Option<ProgressBar> {
    if show {
        Some(create_spinner(message))
    } else {
        None
    }
}

pub fn fail_spinner(spinner: &mut ProgressBar, message: String) {
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{msg:.red}")
            .expect("Failed to create error spinner template"),
    );
    spinner.finish_with_message(format!("✗ {message}"));
}

pub fn success_spinner(spinner: &mut ProgressBar, message: String) {
    spinner.finish_with_message(format!("✓ {message}"));
}
