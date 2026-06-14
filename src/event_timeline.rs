//! Unified coordination timeline for the events overlay (Ctrl+E).

use std::fs;
use std::path::Path;

use crate::dead_letter;
use crate::mailbox;
use crate::meta::{self, CoordEvent};

const LOG_TAIL_LINES: usize = 40;

pub fn build_timeline_lines(project_dir: &Path, max_entries: usize) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(String::from("── events.jsonl ──"));
    let events = meta::load_coord_events(project_dir);
    if events.is_empty() {
        lines.push("  (暂无事件)".into());
    } else {
        let start = events.len().saturating_sub(max_entries);
        for ev in &events[start..] {
            lines.push(format_event(ev));
        }
    }

    lines.push(String::from("── mailbox.jsonl ──"));
    let mailbox = mailbox::load_entries(project_dir, max_entries);
    if mailbox.is_empty() {
        lines.push("  (暂无)".into());
    } else {
        for e in &mailbox {
            lines.push(mailbox::format_entry(e));
        }
    }

    lines.push(String::from("── dead_letter.jsonl ──"));
    let dead = dead_letter::load_dead_letters(project_dir, 12);
    if dead.is_empty() {
        lines.push("  (无)".into());
    } else {
        for r in &dead {
            lines.push(dead_letter::format_dead_letter(r));
        }
    }

    let log_lines = orchestration_log_tail(project_dir);
    if !log_lines.is_empty() {
        lines.push(String::from("── agent-tui.log（协调） ──"));
        lines.extend(log_lines);
    }

    lines
}

fn format_event(ev: &CoordEvent) -> String {
    let time = ev
        .time
        .as_deref()
        .map(short_time)
        .unwrap_or_else(|| "--:--".into());
    let msg = truncate(&ev.message.replace('\n', " "), 72);

    match ev.kind.as_str() {
        "notify" => {
            let to = ev.agent.as_deref().unwrap_or("?");
            if ev.message.contains("【委派·") {
                let task = extract_task_tag(&ev.message).unwrap_or_else(|| "?".into());
                return format!("  {time} 委派 {from} → {to}  「{task}」 {msg}", from = ev.from);
            }
            if ev.message.contains("【回执·") {
                let task = extract_task_tag(&ev.message).unwrap_or_else(|| "?".into());
                return format!("  {time} 回执 {from} → {to}  「{task}」 {msg}", from = ev.from);
            }
            if ev.message.contains("【用户任务】") || ev.message.contains("【协调规则") {
                return format!("  {time} 主Agent {to} ← {from}  {msg}", from = ev.from);
            }
            format!("  {time} 通知 {from} → {to}  {msg}", from = ev.from)
        }
        "claim" => {
            let agent = ev.agent.as_deref().unwrap_or("?");
            let task = ev.task.as_deref().unwrap_or("?");
            let action = ev.action.as_deref().unwrap_or("claim");
            format!("  {time} 认领 {agent} 「{task}」 ({action})")
        }
        "broadcast" => format!("  {time} 广播 {from}  {msg}", from = ev.from),
        other => format!("  {time} {other}  {msg}"),
    }
}

fn extract_task_tag(message: &str) -> Option<String> {
    let start = message.find('·')? + '·'.len_utf8();
    let rest = &message[start..];
    let end = rest.find('·').or_else(|| rest.find('】'))?;
    let task = rest[..end].trim();
    if task.is_empty() {
        None
    } else {
        Some(task.to_string())
    }
}

fn short_time(iso: &str) -> String {
    if iso.len() >= 19 {
        iso[11..19].to_string()
    } else if iso.len() >= 8 {
        iso[iso.len() - 8..].to_string()
    } else {
        iso.to_string()
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let tail: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{tail}…")
}

fn orchestration_log_tail(project_dir: &Path) -> Vec<String> {
    let path = project_dir.join(".agents/agent-tui.log");
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| is_orchestration_log_line(l))
        .rev()
        .take(LOG_TAIL_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|l| format!("  {l}"))
        .collect()
}

fn is_orchestration_log_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    [
        "auto-plan",
        "auto-report",
        "task dag",
        "report gate",
        "派发",
        "回执",
        "briefing",
        "pending dispatch",
        "依赖满足",
        "agent-plan",
        "auto-retry",
        "dead-letter",
        "dead_letter",
        "retry",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_delegate_notify() {
        let ev = CoordEvent {
            line_no: 1,
            time: Some("2026-06-14T12:34:56+08:00".into()),
            kind: "notify".into(),
            agent: Some("codex".into()),
            message: "【委派·auth-api】请立即开始".into(),
            from: "cursor".into(),
            task: None,
            action: None,
        };
        let line = format_event(&ev);
        assert!(line.contains("委派"));
        assert!(line.contains("auth-api"));
    }
}
