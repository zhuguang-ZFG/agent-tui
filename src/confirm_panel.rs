//! Confirm destructive inbox ops before execution (safe ops gate).

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::ops::{self, OpsVerb};

pub struct ConfirmKeyOutcome {
    pub consumed: bool,
    pub status: Option<String>,
    pub confirmed: Option<OpsVerb>,
}

impl ConfirmKeyOutcome {
    pub fn not_handled() -> Self {
        Self {
            consumed: false,
            status: None,
            confirmed: None,
        }
    }

    pub fn handled(status: Option<String>) -> Self {
        Self {
            consumed: true,
            status,
            confirmed: None,
        }
    }
}

pub struct ConfirmPanel {
    pub open: bool,
    pub verb: Option<OpsVerb>,
    lines: Vec<String>,
}

impl ConfirmPanel {
    pub fn closed() -> Self {
        Self {
            open: false,
            verb: None,
            lines: Vec::new(),
        }
    }

    pub fn open_for(&mut self, verb: OpsVerb) {
        self.lines = ops::describe_confirm(&verb)
            .lines()
            .map(str::to_string)
            .collect();
        self.open = true;
        self.verb = Some(verb);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.verb = None;
        self.lines.clear();
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> ConfirmKeyOutcome {
        if !self.open {
            return ConfirmKeyOutcome::not_handled();
        }

        match code {
            KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                self.close();
                ConfirmKeyOutcome::handled(Some("已取消".into()))
            }
            KeyCode::Enter
            | KeyCode::Char('y' | 'Y')
                if !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let verb = self.verb.take();
                self.close();
                if let Some(v) = verb {
                    ConfirmKeyOutcome {
                        consumed: true,
                        status: Some("已确认，正在执行…".into()),
                        confirmed: Some(v),
                    }
                } else {
                    ConfirmKeyOutcome::handled(Some("无待确认操作".into()))
                }
            }
            _ => ConfirmKeyOutcome::handled(None),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        if !self.open {
            return;
        }

        f.render_widget(Clear, area);

        let modal = centered_rect(68, 45, area);
        let block = Block::default()
            .title(" ⚠ 确认操作 ")
            .borders(Borders::ALL)
            .border_style(
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(modal);

        let styled: Vec<Line> = self
            .lines
            .iter()
            .map(|line| {
                let style = if line.starts_with('⚠') {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if line.starts_with('将') || line.contains("危险") {
                    Style::default().fg(Color::LightRed)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(Span::styled(line.clone(), style))
            })
            .collect();

        let footer = Line::from(vec![
            Span::styled("Y / Enter", Style::default().fg(Color::Green)),
            Span::raw(" 执行    "),
            Span::styled("N / Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" 取消"),
        ]);

        let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(inner);
        f.render_widget(
            Paragraph::new(styled).wrap(Wrap { trim: true }),
            chunks[0],
        );
        f.render_widget(Paragraph::new(footer), chunks[1]);
        f.render_widget(block, modal);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}
