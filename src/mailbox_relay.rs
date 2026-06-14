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
    let task = entry.task.as_deref()?;
    match entry.kind.as_str() {
        "delegate" => Some(format!("delegate:{}:{}", entry.to, task)),
        "report" => Some(format!("report:{}:{}", entry.from, task)),
        "user_task" => Some(format!("user_task:{}:{}", entry.to, entry.time)),
        _ => None,
    }
}

pub fn dedupe_key_for_notify(message: &str, target: &str, from: &str) -> Option<String> {
    if message.contains("【委派·") {
        let task = extract_task(message)?;
        return Some(format!("delegate:{target}:{task}"));
    }
    if message.contains("【回执·") {
        let task = extract_task(message)?;
        return Some(format!("report:{from}:{task}"));
    }
    if message.contains("【用户任务】") {
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
    for (offset, entry) in entries.iter().enumerate() {
        state.mailbox_line += 1;
        if !should_relay_kind(&entry.kind) {
            continue;
        }
        let Some(key) = dedupe_key_for_mailbox(entry) else {
            continue;
        };
        if state.dedupe_keys.contains(&key) {
            continue;
        }
        let Some(idx) = agent_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&entry.to))
        else {
            continue;
        };
        if agent_names
            .get(idx)
            .is_some_and(|n| n.eq_ignore_ascii_case(&entry.from))
        {
            continue;
        }
        let Some(pane) = panes.get(idx).and_then(|p| p.as_ref()) else {
            continue;
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
        state.dedupe_keys.insert(key);
        delivered += 1;
        let _ = offset;
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
            dedupe_key_for_notify(msg, "codex", "cursor")
        );
    }
}
