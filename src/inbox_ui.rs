use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::claims;
use crate::delegation;
use crate::lead_watch;
use crate::meta::{self, InboxCommand};

pub struct InboxKeyOutcome {
    pub consumed: bool,
    pub status: Option<String>,
}

impl InboxKeyOutcome {
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

pub struct InboxPanel {
    pub open: bool,
    lines: Vec<String>,
    scroll: usize,
    input: String,
}

impl InboxPanel {
    pub fn closed() -> Self {
        Self {
            open: false,
            lines: Vec::new(),
            scroll: 0,
            input: String::new(),
        }
    }

    pub fn toggle(&mut self, project_dir: &Path) {
        self.open = !self.open;
        if self.open {
            self.refresh(project_dir);
            self.scroll = 0;
        } else {
            self.input.clear();
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.input.clear();
    }

    pub fn refresh(&mut self, project_dir: &Path) {
        self.lines = meta::load_shared_inbox_lines(project_dir);
        if self.scroll > self.lines.len().saturating_sub(1) {
            self.scroll = self.lines.len().saturating_sub(1);
        }
    }

    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        project_dir: &Path,
        agent_names: &[String],
        focused_agent: Option<&str>,
        lead_agent: &str,
    ) -> InboxKeyOutcome {
        if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('i' | 'I')) {
            self.toggle(project_dir);
            return InboxKeyOutcome {
                consumed: true,
                status: Some(if self.open {
                    "留言板".into()
                } else {
                    String::new()
                }),
            };
        }

        if !self.open {
            return InboxKeyOutcome::not_handled();
        }

        match code {
            KeyCode::Esc => {
                self.close();
                InboxKeyOutcome::handled(None)
            }
            KeyCode::Enter => {
                let raw = std::mem::take(&mut self.input);
                let result = match meta::parse_inbox_command(
                    &raw,
                    agent_names,
                    focused_agent,
                    lead_agent,
                ) {
                    Ok(InboxCommand::TaskToLead { message }) => lead_watch::submit_user_task(
                        project_dir,
                        lead_agent,
                        message,
                    )
                    .map(|_| format!("已提交主 Agent 拆解：{message}")),
                    Ok(InboxCommand::Delegate {
                        worker,
                        task,
                        description,
                    }) => delegation::delegate_task(
                        project_dir,
                        lead_agent,
                        worker,
                        task,
                        description,
                    )
                    .map(|_| format!("已委派 {worker} 任务「{task}」")),
                    Ok(InboxCommand::Report {
                        lead,
                        task,
                        message,
                    }) => {
                        let reporter = focused_agent.unwrap_or("user");
                        delegation::report_task(project_dir, reporter, lead, task, message)
                            .map(|_| format!("已回执给 {lead}：{message}"))
                    }
                    Ok(InboxCommand::Claim { agent, task }) => {
                        claims::claim_task(project_dir, agent, task)
                            .map(|_| format!("{agent} 已认领任务「{task}」"))
                    }
                    Ok(InboxCommand::Release { agent, task }) => {
                        claims::release_task(project_dir, agent, task)
                            .map(|_| format!("{agent} 已释放任务「{task}」"))
                    }
                    Ok(InboxCommand::Notify { agent, body }) => meta::notify_agent(project_dir, agent, body)
                        .map(|_| format!("已通知 {agent}：{body}")),
                    Ok(InboxCommand::Broadcast(body)) => meta::append_shared_message(project_dir, body)
                        .map(|_| format!("已广播：{body}")),
                    Err(e) => Err(e),
                };
                match result {
                    Ok(status) => {
                        self.refresh(project_dir);
                        self.scroll = self.lines.len().saturating_sub(1);
                        InboxKeyOutcome::handled(Some(status))
                    }
                    Err(e) => InboxKeyOutcome::handled(Some(format!("写入失败：{e:#}"))),
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                InboxKeyOutcome::handled(None)
            }
            KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(c);
                InboxKeyOutcome::handled(None)
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                InboxKeyOutcome::handled(None)
            }
            KeyCode::Down => {
                if self.scroll + 1 < self.lines.len() {
                    self.scroll += 1;
                }
                InboxKeyOutcome::handled(None)
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                InboxKeyOutcome::handled(None)
            }
            KeyCode::PageDown => {
                self.scroll = (self.scroll + 10).min(self.lines.len().saturating_sub(1));
                InboxKeyOutcome::handled(None)
            }
            KeyCode::Home => {
                self.scroll = 0;
                InboxKeyOutcome::handled(None)
            }
            KeyCode::End => {
                self.scroll = self.lines.len().saturating_sub(1);
                InboxKeyOutcome::handled(None)
            }
            _ => InboxKeyOutcome::handled(None),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        if !self.open {
            return;
        }

        f.render_widget(Clear, area);

        let block = Block::default()
            .title(" 留言板  Esc关闭 ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta))
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(area);
        let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(inner);

        let visible_rows = chunks[0].height.saturating_sub(1) as usize;
        let total = self.lines.len();
        let start = if self.scroll > 0 {
            self.scroll.min(total.saturating_sub(visible_rows.max(1)))
        } else {
            total.saturating_sub(visible_rows.max(1))
        };

        let content: Vec<Line> = if self.lines.is_empty() {
            vec![Line::from(Span::styled(
                "（暂无留言，直接输入后按 Enter）",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.lines[start..]
                .iter()
                .map(|line| {
                    Line::from(Span::styled(
                        truncate_line(line, chunks[0].width.saturating_sub(2) as usize),
                        line_style(line),
                    ))
                })
                .collect()
        };

        f.render_widget(
            Paragraph::new(content).style(Style::default().bg(Color::Black)),
            chunks[0],
        );
        render_input(f, chunks[1], &self.input);
        f.render_widget(block, area);
    }
}

fn line_style(line: &str) -> Style {
    if line.contains('→') {
        Style::default().fg(Color::Cyan)
    } else if line.contains("认领") || line.contains("释放") {
        Style::default().fg(Color::Yellow)
    } else if line.contains("冲突") {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::White)
    }
}

fn truncate_line(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    if s.chars().count() <= max_cols {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_cols.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn render_input(f: &mut Frame, area: Rect, input: &str) {
    let prompt = if input.is_empty() {
        "输入消息 / !任务 描述需求，Enter 发送".to_string()
    } else {
        format!("> {input}_")
    };
    let style = if input.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    };
    f.render_widget(Paragraph::new(prompt).style(style), area);
}
