//! Coordination event timeline overlay (Ctrl+E).

use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::event_timeline;

pub struct EventsKeyOutcome {
    pub consumed: bool,
    pub status: Option<String>,
}

impl EventsKeyOutcome {
    pub fn not_handled() -> Self {
        Self {
            consumed: false,
            status: None,
        }
    }

    pub fn handled(status: Option<String>) -> Self {
        Self {
            consumed: true,
            status,
        }
    }
}

pub struct EventsPanel {
    pub open: bool,
    lines: Vec<String>,
    scroll: usize,
}

impl EventsPanel {
    pub fn closed() -> Self {
        Self {
            open: false,
            lines: Vec::new(),
            scroll: 0,
        }
    }

    pub fn toggle(&mut self, project_dir: &Path) {
        self.open = !self.open;
        if self.open {
            self.refresh(project_dir);
            self.scroll = self.lines.len().saturating_sub(1);
        }
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn refresh(&mut self, project_dir: &Path) {
        self.lines = event_timeline::build_timeline_lines(project_dir, 200);
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let total = self.lines.len();
        if self.scroll > total.saturating_sub(1) {
            self.scroll = total.saturating_sub(1);
        }
    }

    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        project_dir: &Path,
    ) -> EventsKeyOutcome {
        if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('e' | 'E')) {
            self.toggle(project_dir);
            let status = if self.open {
                Some(format!("事件时间线（{} 行）", self.lines.len()))
            } else {
                None
            };
            return EventsKeyOutcome::handled(status);
        }

        if !self.open {
            return EventsKeyOutcome::not_handled();
        }

        match code {
            KeyCode::Esc => {
                self.close();
                EventsKeyOutcome::handled(None)
            }
            KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.refresh(project_dir);
                self.scroll = self.lines.len().saturating_sub(1);
                EventsKeyOutcome::handled(Some("已刷新事件时间线".into()))
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                EventsKeyOutcome::handled(None)
            }
            KeyCode::Down => {
                if self.scroll + 1 < self.lines.len() {
                    self.scroll += 1;
                }
                EventsKeyOutcome::handled(None)
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(12);
                EventsKeyOutcome::handled(None)
            }
            KeyCode::PageDown => {
                self.scroll = (self.scroll + 12).min(self.lines.len().saturating_sub(1));
                EventsKeyOutcome::handled(None)
            }
            KeyCode::Home => {
                self.scroll = 0;
                EventsKeyOutcome::handled(None)
            }
            KeyCode::End => {
                self.scroll = self.lines.len().saturating_sub(1);
                EventsKeyOutcome::handled(None)
            }
            _ => EventsKeyOutcome::handled(None),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        if !self.open {
            return;
        }

        f.render_widget(Clear, area);

        let block = Block::default()
            .title(" 协调事件时间线  Esc关闭 ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green))
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(area);

        let visible_rows = inner.height.saturating_sub(1) as usize;
        let total = self.lines.len();
        let start = if self.scroll > 0 {
            self.scroll.min(total.saturating_sub(visible_rows.max(1)))
        } else {
            total.saturating_sub(visible_rows.max(1))
        };
        let end = (start + visible_rows).min(total);

        let styled: Vec<Line> = if self.lines.is_empty() {
            vec![Line::from(Span::styled(
                "（暂无事件）",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.lines[start..end]
                .iter()
                .map(|line| {
                    let style = if line.starts_with("──") {
                        Style::default().fg(Color::Yellow)
                    } else if line.contains("委派") {
                        Style::default().fg(Color::Cyan)
                    } else if line.contains("回执") {
                        Style::default().fg(Color::Magenta)
                    } else if line.contains("认领") {
                        Style::default().fg(Color::Green)
                    } else if line.contains("[warn]") {
                        Style::default().fg(Color::Yellow)
                    } else if line.contains("[error]") {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default()
                    };
                    Line::from(Span::styled(line.clone(), style))
                })
                .collect()
        };

        let list = Paragraph::new(styled).style(Style::default().bg(Color::Black));
        f.render_widget(list, inner);
        f.render_widget(block, area);
    }
}
