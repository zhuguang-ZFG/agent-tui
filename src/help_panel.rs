//! 速查与进度 overlay（Ctrl+G / Ctrl+N）— 子命令在 TUI 内的视图。

use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::guide;
use crate::workflow_phase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpView {
    Guide,
    Next,
}

pub struct HelpKeyOutcome {
    pub consumed: bool,
    pub status: Option<String>,
}

impl HelpKeyOutcome {
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

pub struct HelpPanel {
    pub open: bool,
    view: HelpView,
    lines: Vec<String>,
    scroll: usize,
}

impl HelpPanel {
    pub fn closed() -> Self {
        Self {
            open: false,
            view: HelpView::Guide,
            lines: Vec::new(),
            scroll: 0,
        }
    }

    pub fn open_guide(&mut self, project_dir: &Path) {
        self.open = true;
        self.view = HelpView::Guide;
        self.refresh(project_dir);
        self.scroll = 0;
    }

    pub fn open_next(&mut self, project_dir: &Path) {
        self.open = true;
        self.view = HelpView::Next;
        self.refresh(project_dir);
        self.scroll = 0;
    }

    pub fn toggle_guide(&mut self, project_dir: &Path) {
        if self.open && self.view == HelpView::Guide {
            self.close();
        } else {
            self.open_guide(project_dir);
        }
    }

    pub fn toggle_next(&mut self, project_dir: &Path) {
        if self.open && self.view == HelpView::Next {
            self.close();
        } else {
            self.open_next(project_dir);
        }
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn refresh(&mut self, project_dir: &Path) {
        let text = match self.view {
            HelpView::Guide => guide::cheat_sheet(Some(project_dir)),
            HelpView::Next => workflow_phase::format_next_report(&workflow_phase::evaluate(project_dir)),
        };
        self.lines = text.lines().map(str::to_string).collect();
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let total = self.lines.len();
        if self.scroll > total.saturating_sub(1) {
            self.scroll = total.saturating_sub(1);
        }
    }

    fn title(&self) -> &'static str {
        match self.view {
            HelpView::Guide => " 速查 Ctrl+G  Esc关闭 ",
            HelpView::Next => " 当前进度 Ctrl+N  Esc关闭 ",
        }
    }

    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        project_dir: &Path,
    ) -> HelpKeyOutcome {
        if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('g' | 'G')) {
            self.toggle_guide(project_dir);
            let status = if self.open && self.view == HelpView::Guide {
                Some("速查".into())
            } else {
                None
            };
            return HelpKeyOutcome::handled(status);
        }
        if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('n' | 'N')) {
            self.toggle_next(project_dir);
            let status = if self.open && self.view == HelpView::Next {
                Some(workflow_phase::short_label(project_dir))
            } else {
                None
            };
            return HelpKeyOutcome::handled(status);
        }

        if !self.open {
            return HelpKeyOutcome::not_handled();
        }

        match code {
            KeyCode::Esc => {
                self.close();
                HelpKeyOutcome::handled(None)
            }
            KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.refresh(project_dir);
                HelpKeyOutcome::handled(Some("已刷新".into()))
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                HelpKeyOutcome::handled(None)
            }
            KeyCode::Down => {
                if self.scroll + 1 < self.lines.len() {
                    self.scroll += 1;
                }
                HelpKeyOutcome::handled(None)
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(12);
                HelpKeyOutcome::handled(None)
            }
            KeyCode::PageDown => {
                self.scroll = (self.scroll + 12).min(self.lines.len().saturating_sub(1));
                HelpKeyOutcome::handled(None)
            }
            KeyCode::Home => {
                self.scroll = 0;
                HelpKeyOutcome::handled(None)
            }
            KeyCode::End => {
                self.scroll = self.lines.len().saturating_sub(1);
                HelpKeyOutcome::handled(None)
            }
            _ => HelpKeyOutcome::handled(None),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        if !self.open {
            return;
        }

        f.render_widget(Clear, area);

        let color = match self.view {
            HelpView::Guide => Color::Blue,
            HelpView::Next => Color::LightGreen,
        };
        let block = Block::default()
            .title(self.title())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(area);

        let visible_rows = inner.height.saturating_sub(1) as usize;
        let total = self.lines.len();
        let start = self
            .scroll
            .min(total.saturating_sub(visible_rows.max(1)));
        let end = (start + visible_rows).min(total);

        let styled: Vec<Line> = if self.lines.is_empty() {
            vec![Line::from(Span::styled(
                "（空）",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.lines[start..end]
                .iter()
                .map(|line| {
                    let style = if line.starts_with('─') {
                        Style::default().fg(Color::Yellow)
                    } else if line.starts_with("当前阶段") || line.starts_with("你现在只需") {
                        Style::default()
                            .fg(Color::LightGreen)
                            .add_modifier(Modifier::BOLD)
                    } else if line.starts_with("详情") || line.starts_with("命令") || line.starts_with("TUI")
                        || line.contains('!') || line.contains("Ctrl+")
                    {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    };
                    Line::from(Span::styled(line.clone(), style))
                })
                .collect()
        };

        f.render_widget(Paragraph::new(styled).style(Style::default().bg(Color::Black)), inner);
        f.render_widget(block, area);
    }
}
