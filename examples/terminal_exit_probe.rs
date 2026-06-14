//! Quick probe: init TUI, one blank frame, restore, print marker for shell sanity.
//! Run: cargo run --release --example terminal_exit_probe

use std::io::Write;
use std::time::Duration;

use ratatui::widgets::Paragraph;

fn main() -> std::io::Result<()> {
    let _guard = agent_tui_terminal::TerminalGuard::enter();
    let mut terminal = ratatui::init();
    terminal.draw(|f| {
        let area = f.area();
        f.render_widget(Paragraph::new("terminal_exit_probe"), area);
    })?;
    std::thread::sleep(Duration::from_millis(100));
    agent_tui_terminal::restore_host_terminal();
    let mut out = std::io::stdout();
    writeln!(out, "TERMINAL_PROBE_OK")?;
    out.flush()?;
    Ok(())
}

// Minimal copy of terminal module for example (avoid circular dep)
mod agent_tui_terminal {
    use std::io::{self, Write};
    use crossterm::cursor::Show;
    use crossterm::terminal::{self, Clear, ClearType};
    use crossterm::execute;

    pub fn restore_host_terminal() {
        let _ = ratatui::try_restore();
        let mut out = io::stdout();
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            out,
            terminal::LeaveAlternateScreen,
            Show,
            crossterm::cursor::MoveTo(0, 0),
            Clear(ClearType::FromCursorDown),
        );
        let _ = out.flush();
        let _ = writeln!(out);
        let _ = out.flush();
    }

    pub struct TerminalGuard(bool);
    impl TerminalGuard {
        pub fn enter() -> Self {
            Self(true)
        }
    }
    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            if self.0 {
                restore_host_terminal();
            }
        }
    }
}
