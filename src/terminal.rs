use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use crossterm::cursor::{MoveTo, Show};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::execute;

/// Restore the hosting console after TUI (normal exit, panic, or drop).
pub fn restore_host_terminal() {
    let _ = ratatui::try_restore();
    let mut out = io::stdout();
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        out,
        terminal::LeaveAlternateScreen,
        Show,
        MoveTo(0, 0),
        Clear(ClearType::FromCursorDown),
    );
    let _ = out.flush();
    let _ = writeln!(out);
    let _ = out.flush();
}

/// RAII guard: always restore console when TUI session ends.
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    pub fn enter() -> Self {
        Self { active: true }
    }

    pub fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.active {
            restore_host_terminal();
        }
    }
}

/// Write runtime errors to log instead of stderr (stderr corrupts the live TUI).
pub fn log_message(project_dir: &Path, level: &str, message: &str) {
    let path = project_dir.join(".agents/agent-tui.log");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "[{level}] {message}");
    }
}
