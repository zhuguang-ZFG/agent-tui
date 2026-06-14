//! Task board overlay (Ctrl+T) with optional memory FTS search.

use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::memory_fts;
use crate::task_board;

pub struct TasksKeyOutcome {
    pub consumed: bool,
    pub status: Option<String>,
}

impl TasksKeyOutcome {
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

enum PanelMode {
    Board,
    SearchResults,
}

pub struct TasksPanel {
    pub open: bool,
    board_lines: Vec<String>,
    result_lines: Vec<String>,
    scroll: usize,
    search_input: String,
    mode: PanelMode,
}

impl TasksPanel {
    pub fn closed() -> Self {
        Self {
            open: false,
            board_lines: Vec::new(),
            result_lines: Vec::new(),
            scroll: 0,
            search_input: String::new(),
            mode: PanelMode::Board,
        }
    }

    pub fn toggle(&mut self, project_dir: &Path) {
        self.open = !self.open;
        if self.open {
            self.refresh_board(project_dir);
            self.mode = PanelMode::Board;
            self.result_lines.clear();
            self.scroll = 0;
        } else {
            self.search_input.clear();
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.search_input.clear();
    }

    pub fn refresh_board(&mut self, project_dir: &Path) {
        self.board_lines = task_board::format_task_board(project_dir)
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_else(|e| vec![format!("加载失败：{e:#}")]);
        self.clamp_scroll();
    }

    fn display_lines(&self) -> &[String] {
        match self.mode {
            PanelMode::Board => &self.board_lines,
            PanelMode::SearchResults => &self.result_lines,
        }
    }

    fn clamp_scroll(&mut self) {
        let total = self.display_lines().len();
        if self.scroll > total.saturating_sub(1) {
            self.scroll = total.saturating_sub(1);
        }
    }

    fn run_search(&mut self, project_dir: &Path) -> TasksKeyOutcome {
        let q = self.search_input.trim();
        if q.is_empty() {
            self.mode = PanelMode::Board;
            self.result_lines.clear();
            self.scroll = 0;
            return TasksKeyOutcome::handled(None);
        }
        match memory_fts::search(project_dir, q, 20) {
            Ok(hits) => {
                self.result_lines = memory_fts::format_hits(&hits);
                self.mode = PanelMode::SearchResults;
                self.scroll = 0;
                let status = format!("记忆搜索「{q}」：{} 条", hits.len());
                TasksKeyOutcome::handled(Some(status))
            }
            Err(e) => TasksKeyOutcome::handled(Some(format!("搜索失败：{e:#}"))),
        }
    }

    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        project_dir: &Path,
    ) -> TasksKeyOutcome {
        if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('t' | 'T')) {
            self.toggle(project_dir);
            let status = if self.open {
                Some("任务看板".into())
            } else {
                None
            };
            return TasksKeyOutcome::handled(status);
        }

        if !self.open {
            return TasksKeyOutcome::not_handled();
        }

        match code {
            KeyCode::Esc => {
                self.close();
                TasksKeyOutcome::handled(None)
            }
            KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.refresh_board(project_dir);
                let _ = memory_fts::reindex_all(project_dir);
                self.mode = PanelMode::Board;
                self.result_lines.clear();
                TasksKeyOutcome::handled(Some("已刷新任务看板并重建记忆索引".into()))
            }
            KeyCode::Enter => self.run_search(project_dir),
            KeyCode::Backspace => {
                self.search_input.pop();
                TasksKeyOutcome::handled(None)
            }
            KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.search_input.push(c);
                TasksKeyOutcome::handled(None)
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                TasksKeyOutcome::handled(None)
            }
            KeyCode::Down => {
                let total = self.display_lines().len();
                if self.scroll + 1 < total {
                    self.scroll += 1;
                }
                TasksKeyOutcome::handled(None)
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                TasksKeyOutcome::handled(None)
            }
            KeyCode::PageDown => {
                self.scroll = (self.scroll + 10).min(self.display_lines().len().saturating_sub(1));
                TasksKeyOutcome::handled(None)
            }
            KeyCode::Home => {
                self.scroll = 0;
                TasksKeyOutcome::handled(None)
            }
            KeyCode::End => {
                self.scroll = self.display_lines().len().saturating_sub(1);
                TasksKeyOutcome::handled(None)
            }
            KeyCode::Tab => {
                self.mode = PanelMode::Board;
                self.result_lines.clear();
                self.clamp_scroll();
                TasksKeyOutcome::handled(Some("返回任务看板".into()))
            }
            _ => TasksKeyOutcome::handled(None),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        if !self.open {
            return;
        }

        f.render_widget(Clear, area);

        let title = match self.mode {
            PanelMode::Board => " 任务看板  Esc关闭 ",
            PanelMode::SearchResults => " 记忆搜索结果  Tab返回看板 ",
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(area);
        let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(inner);

        let visible_rows = chunks[0].height.saturating_sub(1) as usize;
        let lines = self.display_lines();
        let total = lines.len();
        let start = if self.scroll > 0 {
            self.scroll.min(total.saturating_sub(visible_rows.max(1)))
        } else {
            0
        };
        let end = (start + visible_rows).min(total);

        let styled: Vec<Line> = lines[start..end]
            .iter()
            .map(|line| {
                let style = if line.starts_with("──") {
                    Style::default().fg(Color::Yellow)
                } else if line.contains('⚡') || line.contains("冲突") {
                    Style::default().fg(Color::Red)
                } else if line.starts_with("  ✓") {
                    Style::default().fg(Color::Green)
                } else if line.starts_with('[') {
                    Style::default().fg(Color::Magenta)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(line.clone(), style))
            })
            .collect();

        let list = Paragraph::new(styled).style(Style::default().bg(Color::Black));
        let hint = if self.search_input.is_empty() {
            "记忆搜索（Enter）"
        } else {
            &self.search_input
        };
        let input = Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::styled(
                hint,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(Color::DarkGray));

        f.render_widget(list, chunks[0]);
        f.render_widget(input, chunks[1]);
        f.render_widget(block, area);
    }
}