use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::time::Duration;

use crate::consts::TICK_STRING;

/// Half-shaded rotating-cube glyphs: the lit face moves top → right → bottom →
/// left, reading like a tumbling cube.
const CUBE_HALF_FRAMES: [&str; 4] = ["⬒", "◨", "⬓", "◧"];

const SHIMMER_DIM: (u8, u8, u8) = (120, 128, 140);
const SHIMMER_BRIGHT: (u8, u8, u8) = (170, 225, 255);

fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

/// A single-line animated "shimmer" spinner: a rotating half-shaded cube, the
/// label with a light gradient sweeping left→right across it, and trailing dots
/// that build up one at a time on a loop. The whole animation is precomputed as
/// indicatif tick frames so the message itself animates (not just a glyph).
/// `colored` auto-disables ANSI when stdout isn't a TTY, so this degrades to
/// plain text in pipes/CI.
pub fn create_shimmer_spinner(label: &str) -> ProgressBar {
    let chars: Vec<char> = label.chars().collect();
    let n = chars.len();
    // One sweep: highlight enters from off the left edge and exits past the end.
    let sweep = n + 6;

    let frames: Vec<String> = (0..sweep)
        .map(|i| {
            let center = i as isize - 3;
            let cube = CUBE_HALF_FRAMES[i % 4];
            let dots = ".".repeat((i / 2) % 4); // 0,1,2,3 building up, slower than the sweep
            let mut text = String::new();
            for (c, ch) in chars.iter().enumerate() {
                let dist = (c as isize - center).abs();
                let intensity = (1.0 - dist as f32 / 3.0).clamp(0.0, 1.0);
                let (r, g, b) = lerp_rgb(SHIMMER_DIM, SHIMMER_BRIGHT, intensity);
                text.push_str(&ch.to_string().truecolor(r, g, b).to_string());
            }
            let (cr, cg, cb) = SHIMMER_BRIGHT;
            format!("{} {text}{dots}", cube.truecolor(cr, cg, cb))
        })
        // Reserved trailing "finished" frame (never shown — we finish_and_clear).
        .chain(std::iter::once(label.to_string()))
        .collect();

    let frame_refs: Vec<&str> = frames.iter().map(String::as_str).collect();
    let spinner = ProgressBar::new_spinner().with_style(
        ProgressStyle::default_spinner()
            .tick_strings(&frame_refs)
            .template("{spinner}")
            .expect("Failed to create shimmer spinner template"),
    );

    spinner.enable_steady_tick(Duration::from_millis(90));
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
