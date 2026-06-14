//! PTY injection sourced from mailbox.jsonl (OmO-style), with dedupe vs events relay.

use std::time::{Duration, Instant};

use crate::mailbox::{self, MailboxEntry};
use crate::pane::AgentPane;
use crate::relay::{RelayConfig, RelayState};

pub fn relay_source() -> RelaySource {
    match std::env::var("AGENT_TUI_RELAY_SOURCE")
        .unwrap_or_else(|_| "both".into())
        .to_lowercase()
        .as_str()
    {
        "events" => RelaySource::EventsOnly,
        "mailbox" => RelaySource::MailboxOnly,
        _ => RelaySource::Both,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaySource {
    EventsOnly,
    MailboxOnly,
    Both,
}

pub fn mailbox_relay_enabled() -> bool {
    relay_source() != RelaySource::EventsOnly
        && std::env::var("AGENT_TUI_MAILBOX_RELAY")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true)
}

pub fn events_relay_enabled() -> bool {
    relay_source() != RelaySource::MailboxOnly
}

/// Dedupe key shared with events relay to avoid double PTY inject.
pub fn dedupe_key_for_mailbox(entry: &MailboxEntry) -> Option<String> {
    match entry.kind.as_str() {
        "delegate" => {
            let task = entry.task.as_deref()?;
            Some(format!("delegate:{}:{}", entry.to, task))
        }
        "report" => {
            let task = entry.task.as_deref()?;
            Some(format!("report:{}:{}", entry.from, task))
        }
        "user_task" => Some(format!("user_task:{}:{}", entry.to, entry.time)),
        _ => None,
    }
}

pub fn dedupe_key_for_notify(message: &str, target: &str, from: &str, time: Option<&str>) -> Option<String> {
    if message.contains("【委派·") {
        let task = extract_task(message)?;
        return Some(format!("delegate:{target}:{task}"));
    }
    if message.contains("【回执·") {
        let task = extract_task(message)?;
        return Some(format!("report:{from}:{task}"));
    }
    if message.contains("【用户任务】") {
        if let Some(t) = time.filter(|s| !s.is_empty()) {
            return Some(format!("user_task:{target}:{t}"));
        }
        return Some(format!("user_task:{target}:{from}"));
    }
    None
}

fn extract_task(message: &str) -> Option<String> {
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

#[allow(dead_code)]
pub fn register_dedupe(state: &mut RelayState, key: String) {
    state.dedupe_keys.insert(key);
}

pub fn format_mailbox_injection(entry: &MailboxEntry) -> String {
    match entry.kind.as_str() {
        "delegate" => {
            let task = entry.task.as_deref().unwrap_or("?");
            format!(
                "[协调/{from}->{to}] 【委派·{task}】{body}",
                from = entry.from,
                to = entry.to,
                body = entry.body
            )
        }
        "report" => {
            let task = entry.task.as_deref().unwrap_or("?");
            format!(
                "[协调/{from}->{to}] 【回执·{task}】{body}",
                from = entry.from,
                to = entry.to,
                body = entry.body
            )
        }
        "user_task" => format!(
            "[协调/user->{to}] 【用户任务】{body}",
            to = entry.to,
            body = entry.body
        ),
        other => format!(
            "[协调/{from}->{to}] [{other}] {body}",
            from = entry.from,
            to = entry.to,
            body = entry.body
        ),
    }
}

/// Deliver new mailbox entries into agent PTYs.
pub fn dispatch_from_mailbox(
    config: &RelayConfig,
    state: &mut RelayState,
    agent_names: &[String],
    panes: &mut [Option<AgentPane>],
    project_dir: &std::path::Path,
) -> usize {
    if !config.enabled || !mailbox_relay_enabled() {
        return 0;
    }

    let entries = mailbox::load_entries_from_line(project_dir, state.mailbox_line);
    if entries.is_empty() {
        return 0;
    }

    let mut delivered = 0usize;
    for entry in entries.iter() {
        if !should_relay_kind(&entry.kind) {
            state.mailbox_line += 1;
            continue;
        }
        let Some(key) = dedupe_key_for_mailbox(entry) else {
            state.mailbox_line += 1;
            continue;
        };
        if state.dedupe_keys.contains(&key) {
            state.mailbox_line += 1;
            continue;
        }
        let Some(idx) = agent_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&entry.to))
        else {
            state.mailbox_line += 1;
            continue;
        };
        if agent_names
            .get(idx)
            .is_some_and(|n| n.eq_ignore_ascii_case(&entry.from))
        {
            state.mailbox_line += 1;
            continue;
        }
        let Some(pane) = panes.get(idx).and_then(|p| p.as_ref()) else {
            // PTY not ready — do not advance; retry on next tick.
            break;
        };
        let line = format_mailbox_injection(entry);
        pane.inject_line(&line);
        if entry.kind == "delegate" {
            let t0 = Instant::now();
            state
                .wake_after
                .push((idx, t0 + Duration::from_millis(700)));
            state
                .wake_after
                .push((idx, t0 + Duration::from_millis(2000)));
        }
        state.remember_dedupe(&key);
        state.mailbox_line += 1;
        delivered += 1;
    }
    delivered
}

fn should_relay_kind(kind: &str) -> bool {
    matches!(kind, "delegate" | "report" | "user_task")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_keys_match() {
        let entry = MailboxEntry {
            time: "t".into(),
            from: "cursor".into(),
            to: "codex".into(),
            kind: "delegate".into(),
            task: Some("auth-api".into()),
            body: "work".into(),
            source: "x".into(),
        };
        let msg = "【委派·auth-api】请开始";
        assert_eq!(
            dedupe_key_for_mailbox(&entry),
            dedupe_key_for_notify(msg, "codex", "cursor", Some("t"))
        );
    }

    #[test]
    fn user_task_dedupe_uses_time_when_present() {
        let k1 = dedupe_key_for_notify(
            "【用户任务】x",
            "cursor",
            "user",
            Some("2026-01-01"),
        );
        assert_eq!(k1.as_deref(), Some("user_task:cursor:2026-01-01"));
        let entry = MailboxEntry {
            time: "2026-01-01".into(),
            from: "user".into(),
            to: "cursor".into(),
            kind: "user_task".into(),
            task: None,
            body: "x".into(),
            source: "x".into(),
        };
        assert_eq!(dedupe_key_for_mailbox(&entry), k1);
    }
}
