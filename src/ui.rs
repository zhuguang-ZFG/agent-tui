use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::health::HealthState;
use crate::meta::AgentMeta;
use crate::pane::AgentPane;
use crate::workflow_phase;

fn pane_rects(area: ratatui::layout::Rect, count: usize, fullscreen: Option<usize>) -> Vec<(usize, ratatui::layout::Rect)> {
    if let Some(idx) = fullscreen {
        return vec![(idx, area)];
    }
    let rects = grid_rects(area, count);
    rects.into_iter().enumerate().collect()
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

fn health_suffix(health: &HealthState) -> String {
    match health {
        HealthState::Ok => String::new(),
        HealthState::Warn(r) => format!(" ⚠{}", short_label(r, 12)),
        HealthState::Dead(r) => format!(" ✘{}", short_label(r, 12)),
    }
}

fn pane_title(index: usize, name: &str, meta: &AgentMeta, health: &HealthState) -> String {
    let branch = short_label(&meta.branch, 14);
    let unread_badge = if meta.unread > 0 {
        format!(" ●{}", meta.unread)
    } else {
        String::new()
    };
    let conflict = if meta.claim_conflict { " ⚡" } else { "" };
    let tasks = tasks_suffix(meta);
    let health = health_suffix(health);
    format!(
        " {} {} {}{}{}{}{} ",
        index + 1,
        name,
        branch,
        tasks,
        health,
        unread_badge,
        conflict
    )
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn chrome_row_primary(app: &App) -> Line<'static> {
    let phase_color = workflow_phase::badge_color(&app.workflow_phase);
    let mut spans = vec![
        Span::styled(
            app.workflow_badge.clone(),
            Style::default()
                .fg(phase_color)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !app.workflow_hint.is_empty() {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            truncate_chars(&app.workflow_hint, 42),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::raw("  │  "));
    let status_style = if app.status.starts_with("操作失败") || app.status.contains("失败") {
        Style::default().fg(Color::Red)
    } else if app.status.starts_with("正在启动")
        || app.status.contains("已确认")
        || app.status.contains("重试")
    {
        Style::default().fg(Color::Yellow)
    } else if app.status == "就绪" {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    spans.push(Span::styled(
        truncate_chars(&app.status, 72),
        status_style,
    ));
    Line::from(spans)
}

fn shortcut_spans() -> Vec<Span<'static>> {
    vec![
        Span::styled("Ctrl+I", Style::default().fg(Color::Yellow)),
        Span::raw(" 留言 "),
        Span::styled("Ctrl+T", Style::default().fg(Color::Yellow)),
        Span::raw(" 任务 "),
        Span::styled("Ctrl+N", Style::default().fg(Color::Yellow)),
        Span::raw(" 进度 "),
        Span::styled("Ctrl+G", Style::default().fg(Color::Yellow)),
        Span::raw(" 速查 "),
        Span::styled("Ctrl+Q", Style::default().fg(Color::Yellow)),
        Span::raw(" 退出"),
    ]
}

fn chrome_row_secondary(app: &App, solo: bool) -> Line<'static> {
    let mut spans = Vec::new();

    if let Some(secs) = app.spawn_retry_countdown_secs() {
        spans.push(Span::styled(
            format!("启动重试 {secs}s"),
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::raw("  "));
    } else if let Some((done, total)) = app.spawn_progress() {
        spans.push(Span::styled(
            format!("启动 {done}/{total}"),
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::raw("  "));
    } else {
        let alive = app.agents_alive_count();
        let total = app.panes.len();
        if total > 0 && alive < total {
            spans.push(Span::styled(
                format!("{alive}/{total} 在线"),
                Style::default().fg(Color::Red),
            ));
            spans.push(Span::raw(" F5重试  "));
        } else if !app.inbox_line.is_empty() {
            spans.push(Span::styled(
                truncate_chars(&app.inbox_line, 48),
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::raw("  "));
        }
    }

    if solo {
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
        spans.push(Span::styled(
            format!("{name}{dot} ({idx}/{n})"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
        spans.push(Span::styled("Ctrl+Tab", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" 切换  "));
        spans.push(Span::styled("F2", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" 宫格  "));
    } else {
        spans.push(Span::styled("宫格", Style::default().fg(Color::Yellow)));
        let alive = app.agents_alive_count();
        let total = app.panes.len();
        if total > 0 {
            spans.push(Span::raw(format!(" {alive}/{total} ")));
        }
        spans.push(Span::raw("主:"));
        spans.push(Span::styled(
            app.lead_agent.clone(),
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::raw("  "));
        spans.push(Span::styled("Ctrl+1~8", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" 聚焦  "));
        spans.push(Span::styled("Ctrl+Tab", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" 切换  "));
        spans.push(Span::styled("F2", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" 单人  "));
        spans.push(Span::styled("F5", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" 重启  "));
    }

    spans.push(Span::styled("Ctrl+E", Style::default().fg(Color::Yellow)));
    spans.push(Span::raw(" 事件  "));
    spans.extend(shortcut_spans());
    Line::from(spans)
}

fn confirm_status_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled("Y/Enter", Style::default().fg(Color::Green)),
        Span::raw(" 执行  "),
        Span::styled("N/Esc", Style::default().fg(Color::Yellow)),
        Span::raw(" 取消"),
    ])
}

fn inbox_status_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" 发送  "),
        Span::styled("!pr", Style::default().fg(Color::Cyan)),
        Span::raw(" !merge  "),
        Span::styled("!map", Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::raw(" 关闭  "),
        Span::styled("Ctrl+G/N", Style::default().fg(Color::Yellow)),
        Span::raw(" 速查/进度"),
    ])
}

fn help_status_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Yellow)),
        Span::raw(" 滚动  "),
        Span::styled("Ctrl+R", Style::default().fg(Color::Yellow)),
        Span::raw(" 刷新  "),
        Span::styled("Ctrl+G", Style::default().fg(Color::Yellow)),
        Span::raw(" 速查  "),
        Span::styled("Ctrl+N", Style::default().fg(Color::Yellow)),
        Span::raw(" 进度  "),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::raw(" 关闭"),
    ])
}

fn tasks_status_bar(scroll: usize, total: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Yellow)),
        Span::raw(" 移动  "),
        Span::styled("Space", Style::default().fg(Color::Yellow)),
        Span::raw(" 折叠批次  "),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" 搜记忆  "),
        Span::styled("Ctrl+R", Style::default().fg(Color::Yellow)),
        Span::raw(" 刷新  "),
        Span::raw(format!("行 {}/{total} ", scroll + 1)),
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

pub fn draw(f: &mut Frame, app: &mut App) {
    if app.confirm.open {
        let root = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        app.confirm.draw(f, root[0]);
        f.render_widget(Paragraph::new(confirm_status_bar()), root[1]);
        return;
    }

    if app.help.open {
        let root = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        f.render_widget(Clear, root[0]);
        app.help.draw(f, root[0]);
        f.render_widget(Paragraph::new(help_status_bar()), root[1]);
        return;
    }

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
        let (scroll, total) = app.tasks.cursor_info();
        f.render_widget(Paragraph::new(tasks_status_bar(scroll, total)), root[1]);
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

    let root = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(f.area());

    let pane_area = root[0];
    let chrome_primary = root[1];
    let chrome_secondary = root[2];

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
                let cmd = app
                    .agents
                    .get(i)
                    .map(|a| a.command.as_str())
                    .unwrap_or("?");
                let block = ratatui::widgets::Block::default()
                    .title(title)
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(Style::default().fg(Color::Red));
                let reason = match &health {
                    HealthState::Dead(r) => r.as_str(),
                    _ => "启动失败",
                };
                let body = format!(
                    "{reason}\n\ncmd: {cmd}\n\nF5 手动重试  |  Ctrl+I !doctor 体检",
                );
                let msg = Paragraph::new(body)
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(msg, rect);
            }
        }
    }

    f.render_widget(Paragraph::new(chrome_row_primary(app)), chrome_primary);
    f.render_widget(Paragraph::new(chrome_row_secondary(app, solo)), chrome_secondary);
}
