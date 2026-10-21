use std::{
    io::{self, IsTerminal},
    time::Duration,
};

use indicatif::{ProgressBar, ProgressStyle};

/// A bar on stderr when it is a terminal, else hidden: `unit`s done of those planned, a message
/// starting at `start`, and the time so far.
pub fn bar(unit: &str, start: &str) -> ProgressBar {
    if !io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let style = ProgressStyle::with_template(&format!(
        "{{spinner}} {{bar:30}} {{pos}}/{{len}} {unit} · {{msg}} · {{elapsed}}"
    ))
    .expect("a valid template");
    let bar = ProgressBar::no_length()
        .with_style(style)
        .with_message(start.to_string());
    bar.enable_steady_tick(Duration::from_millis(100));
    bar
}
