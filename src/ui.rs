use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::health::HealthState;
use crate::meta::AgentMeta;
use crate::pane::AgentPane;

fn pane_rects(area: ratatui::layout::Rect, count: usize, fullscreen: Option<usize>) -> Vec<(usize, ratatui::layout::Rect)> {
    if let Some(idx) = fullscreen {
        return vec![(idx, area)];
    }
    let rects = grid_rects(area, count);
    rects.into_iter().enumerate().map(|(i, r)| (i, r)).collect()
}

fn grid_rects(area: ratatui::layout::Rect, count: usize) -> Vec<ratatui::layout::Rect> {
    if count <= 1 {
        return vec![area];
    }
    if count == 2 {
        return Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area)
            .to_vec();
    }
    if count == 3 {
        return Layout::horizontal([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(area)
        .to_vec();
    }
    if count == 4 {
        let rows = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
        let top = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[0]);
        let bottom = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[1]);
        return vec![top[0], top[1], bottom[0], bottom[1]];
    }
    let top = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    let top_row = Layout::horizontal([
        Constraint::Percentage(33),
        Constraint::Percentage(34),
        Constraint::Percentage(33),
    ])
    .split(top[0]);
    let bottom_count = count - 3;
    let bottom_row = Layout::horizontal(vec![Constraint::Ratio(1, bottom_count as u32); bottom_count])
        .split(top[1]);
    let mut rects = top_row.to_vec();
    rects.extend_from_slice(&bottom_row);
    rects
}

fn pane_inner_size(area: ratatui::layout::Rect, title: &str, solo: bool) -> (u16, u16) {
    if solo {
        return (area.height.max(8), area.width.max(24));
    }
    let block = ratatui::widgets::Block::default()
        .title(format!(" {title} "))
        .borders(ratatui::widgets::Borders::ALL);
    let inner = block.inner(area);
    (
        inner.height.max(8),
        inner.width.max(24),
    )
}

fn border_style(focused: bool, health: &HealthState, claim_conflict: bool) -> Style {
    if matches!(health, HealthState::Dead(_)) {
        return Style::default().fg(Color::Red);
    }
    if claim_conflict || matches!(health, HealthState::Warn(_)) {
        return Style::default().fg(Color::Yellow);
    }
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn tasks_suffix(meta: &AgentMeta) -> String {
    if meta.tasks.is_empty() {
        return String::new();
    }
    if meta.tasks.len() == 1 {
        format!(" · {}", meta.tasks[0])
    } else {
        format!(" · {}+{}", meta.tasks[0], meta.tasks.len() - 1)
    }
}

fn short_label(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let tail: String = s.chars().rev().take(max.saturating_sub(1)).collect();
    format!("…{}", tail.chars().rev().collect::<String>())
}

fn pane_title(index: usize, name: &str, meta: &AgentMeta, health: &HealthState) -> String {
    let branch = short_label(&meta.branch, 14);
    let badge = match health {
        HealthState::Ok => "",
        HealthState::Warn(_) => " ⚠",
        HealthState::Dead(_) => " ✘",
    };
    let unread_badge = if meta.unread > 0 {
        format!(" ●{}", meta.unread)
    } else {
        String::new()
    };
    let conflict = if meta.claim_conflict { " ⚡" } else { "" };
    let tasks = tasks_suffix(meta);
    format!(
        " {} {} {}{}{}{}{} ",
        index + 1,
        name,
        branch,
        tasks,
        badge,
        unread_badge,
        conflict
    )
}

fn render_solo_pane(f: &mut Frame, pane: &AgentPane, area: ratatui::layout::Rect) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    pane.with_screen(|screen| {
        let widget = tui_term::widget::PseudoTerminal::new(screen);
        f.render_widget(widget, area);
    });
}

fn render_grid_pane(
    f: &mut Frame,
    pane: &AgentPane,
    area: ratatui::layout::Rect,
    focused: bool,
    index: usize,
    meta: &AgentMeta,
    health: &HealthState,
) {
    let title = pane_title(index, &pane.spec.name, meta, health);
    let block = ratatui::widgets::Block::default()
        .title(title)
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(border_style(focused, health, meta.claim_conflict));
    let inner = block.inner(area);
    if inner.width < 3 || inner.height < 2 {
        f.render_widget(block, area);
        return;
    }
    pane.with_screen(|screen| {
        let widget = tui_term::widget::PseudoTerminal::new(screen).block(block);
        f.render_widget(widget, area);
    });
}

fn status_bar_solo(app: &App) -> Line<'static> {
    let name = app
        .agents
        .get(app.focus)
        .map(|a| a.name.as_str())
        .unwrap_or("-");
    let n = app.panes.len();
    let idx = app.focus + 1;
    let unread = app
        .agent_meta(app.focus)
        .map(|m| m.unread)
        .unwrap_or(0);
    let dot = if unread > 0 { " ●" } else { "" };
    Line::from(vec![
        Span::styled(
            format!("{name}{dot} ({idx}/{n})"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("Ctrl+Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" 切换  "),
        Span::styled("F2", Style::default().fg(Color::Yellow)),
        Span::raw(" 四宫格  "),
        Span::styled("Ctrl+I", Style::default().fg(Color::Yellow)),
        Span::raw(" 留言  "),
        Span::styled("Ctrl+T", Style::default().fg(Color::Yellow)),
        Span::raw(" 任务  "),
        Span::styled("Ctrl+E", Style::default().fg(Color::Yellow)),
        Span::raw(" 事件  "),
        Span::styled("Ctrl+Q", Style::default().fg(Color::Yellow)),
        Span::raw(" 退出"),
    ])
}

fn status_bar_grid(app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled("宫格", Style::default().fg(Color::Yellow)),
        Span::raw(" 主:"),
        Span::styled(app.lead_agent.clone(), Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled("Ctrl+1~5", Style::default().fg(Color::Yellow)),
        Span::raw(" 聚焦  "),
        Span::styled("F2", Style::default().fg(Color::Yellow)),
        Span::raw(" 单人  "),
        Span::styled("Ctrl+T", Style::default().fg(Color::Yellow)),
        Span::raw(" 任务  "),
        Span::styled("Ctrl+E", Style::default().fg(Color::Yellow)),
        Span::raw(" 事件  "),
        Span::styled("Ctrl+Q", Style::default().fg(Color::Yellow)),
        Span::raw(" 退出"),
    ])
}

fn inbox_status_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" 发送  "),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::raw(" 关闭留言板"),
    ])
}

fn tasks_status_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" 搜索记忆  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" 返回看板  "),
        Span::styled("Ctrl+R", Style::default().fg(Color::Yellow)),
        Span::raw(" 刷新  "),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::raw(" 关闭"),
    ])
}

fn events_status_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Yellow)),
        Span::raw(" 滚动  "),
        Span::styled("End", Style::default().fg(Color::Yellow)),
        Span::raw(" 最新  "),
        Span::styled("Ctrl+R", Style::default().fg(Color::Yellow)),
        Span::raw(" 刷新  "),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::raw(" 关闭"),
    ])
}

pub fn draw(f: &mut Frame, app: &mut App) {
    if app.events.open {
        let root = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        f.render_widget(Clear, root[0]);
        app.events.draw(f, root[0]);
        f.render_widget(Paragraph::new(events_status_bar()), root[1]);
        return;
    }

    if app.tasks.open {
        let root = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        f.render_widget(Clear, root[0]);
        app.tasks.draw(f, root[0]);
        f.render_widget(Paragraph::new(tasks_status_bar()), root[1]);
        return;
    }

    if app.inbox.open {
        let root = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        f.render_widget(Clear, root[0]);
        app.inbox.draw(f, root[0]);
        f.render_widget(Paragraph::new(inbox_status_bar()), root[1]);
        return;
    }

    let solo = app.is_solo();

    let root = if solo {
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area())
    } else {
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area())
    };

    let pane_area = root[0];
    let status_area = root[1];

    let layout_count = if app.is_solo() {
        1
    } else {
        app.panes.len()
    };
    let placements = pane_rects(pane_area, layout_count, app.fullscreen_index());

    for (i, rect) in placements {
        let agent_name = app
            .agents
            .get(i)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| "-".into());
        let meta = app.agent_meta(i).cloned().unwrap_or_default();
        let health = app.health_state(i);
        let title = pane_title(i, &agent_name, &meta, &health);
        let is_solo_pane = solo && i == app.focus;
        let (inner_h, inner_w) = pane_inner_size(rect, &title, is_solo_pane);
        app.sync_pane_pty_size(i, inner_h, inner_w);

        match app.panes.get(i).and_then(|p| p.as_ref()) {
            Some(pane) if is_solo_pane => render_solo_pane(f, pane, rect),
            Some(pane) => render_grid_pane(f, pane, rect, i == app.focus, i, &meta, &health),
            None => {
                let block = ratatui::widgets::Block::default()
                    .title(title)
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(Style::default().fg(Color::Red));
                let msg = Paragraph::new("启动失败")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(msg, rect);
            }
        }
    }

    let help = if solo {
        status_bar_solo(app)
    } else {
        status_bar_grid(app)
    };
    f.render_widget(Paragraph::new(help), status_area);
}
