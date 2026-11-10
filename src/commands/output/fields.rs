use std::fmt;

use colored::Colorize;

pub(in crate::commands) fn print_field(label: &str, value: &dyn fmt::Display, width: usize) {
    let padded = format!("{label:<width$}");
    println!("{} {value}", padded.dimmed());
}

pub(in crate::commands) fn print_service_environment_context(
    service_name: &str,
    environment_name: &str,
    width: usize,
) {
    print_field("Service:", &service_name.green().bold(), width);
    print_field("Environment:", &environment_name.blue().bold(), width);
}
