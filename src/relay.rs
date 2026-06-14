use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::meta::CoordEvent;
use crate::pane::AgentPane;

pub struct RelayConfig {
    pub enabled: bool,
    pub cooldown: Duration,
}

impl RelayConfig {
    pub fn from_env() -> Self {
        let enabled = std::env::var("AGENT_TUI_RELAY")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let cooldown_secs = std::env::var("AGENT_TUI_RELAY_COOLDOWN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(12);
        Self {
            enabled,
            cooldown: Duration::from_secs(cooldown_secs.max(3)),
        }
    }
}

pub struct RelayState {
    pub cursor: usize,
    pub mailbox_line: usize,
    pub dedupe_keys: HashSet<String>,
    project_dir: Option<std::path::PathBuf>,
    last_sent: Vec<Option<Instant>>,
    /// Deferred Enter injections so workers start after delegate notify.
    pub wake_after: Vec<(usize, Instant)>,
}

impl RelayState {
    pub fn new(project_dir: &std::path::Path, agent_count: usize) -> Self {
        let persisted = crate::coord_dedupe::load_relay_cursor(project_dir);
        Self {
            cursor: persisted.events_cursor,
            mailbox_line: persisted.mailbox_line,
            dedupe_keys: crate::coord_dedupe::load_relay_dedupe(project_dir),
            project_dir: Some(project_dir.to_path_buf()),
            last_sent: vec![None; agent_count],
            wake_after: Vec::new(),
        }
    }

    pub fn persist_with_plan_inbox(&self, plan_inbox_line: usize) {
        if let Some(dir) = self.project_dir.as_ref() {
            crate::coord_dedupe::update_relay_cursor_fields(
                dir,
                self.cursor,
                self.mailbox_line,
                plan_inbox_line,
            );
        }
    }

    pub fn resize(&mut self, agent_count: usize) {
        self.last_sent.resize(agent_count, None);
    }

    pub fn remember_dedupe(&mut self, key: &str) {
        self.dedupe_keys.insert(key.to_string());
        if let Some(dir) = self.project_dir.as_ref() {
            crate::coord_dedupe::remember_relay_dedupe(dir, key);
        }
    }
}

fn auto_wake_enabled() -> bool {
    std::env::var("AGENT_TUI_AUTO_WAKE")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

pub(crate) fn is_delegate_notify(event: &CoordEvent) -> bool {
    event.kind == "notify" && event.message.contains("【委派·")
}

/// Fire due wake prompts (call each tick).
pub fn process_pending_wakes(panes: &mut [Option<AgentPane>], state: &mut RelayState) -> usize {
    let now = Instant::now();
    let mut fired = 0usize;
    state.wake_after.retain(|(idx, at)| {
        if *at > now {
            return true;
        }
        if let Some(pane) = panes.get(*idx).and_then(|p| p.as_ref()) {
            pane.wake_prompt();
            fired += 1;
        }
        false
    });
    fired
}

pub(crate) fn is_lead_briefing_notify(event: &CoordEvent) -> bool {
    if event.kind != "notify" || !event.from.eq_ignore_ascii_case("system") {
        return false;
    }
    let m = event.message.as_str();
    m.contains("【协调规则·")
        || m.contains("【Lead 提醒·")
        || m.contains("【协调规则·Lead")
}

pub fn format_injection(event: &CoordEvent) -> String {
    match event.kind.as_str() {
        "notify" => format!(
            "[协调/{from}->{target}] {msg}",
            from = event.from,
            target = event.agent.as_deref().unwrap_or("?"),
            msg = event.message
        ),
        "broadcast" => format!("[协调/广播/{from}] {msg}", from = event.from, msg = event.message),
        "claim" => {
            let agent = event.agent.as_deref().unwrap_or("?");
            let task = event.task.as_deref().unwrap_or("?");
            match event.action.as_deref() {
                Some("release") => format!("[协调] {agent} 已释放任务「{task}」"),
                _ => format!("[协调] {agent} 认领任务「{task}」"),
            }
        }
        _ => format!("[协调] {}", event.message),
    }
}

/// Deliver pending coordination events into agent PTYs.
pub fn dispatch(
    config: &RelayConfig,
    state: &mut RelayState,
    agent_names: &[String],
    panes: &mut [Option<AgentPane>],
    events: &[CoordEvent],
) -> usize {
    if !config.enabled || !crate::mailbox_relay::events_relay_enabled() {
        state.cursor = events.len();
        return 0;
    }

    let mut delivered = 0usize;
    let pending: Vec<&CoordEvent> = events
        .iter()
        .filter(|e| e.line_no > state.cursor)
        .collect();

    // If a notify is queued for an agent, skip claim relay (notify carries the full instruction).
    let mut notify_targets = HashSet::new();
    for event in &pending {
        if event.kind == "notify" {
            for idx in relay_targets(agent_names, event) {
                notify_targets.insert(idx);
            }
        }
    }

    for event in pending {
        if event.kind == "claim" {
            state.cursor = event.line_no;
            continue;
        }
        if is_lead_briefing_notify(event) {
            state.cursor = event.line_no;
            continue;
        }
        let targets = relay_targets(agent_names, event);
        if targets.is_empty() {
            state.cursor = event.line_no;
            continue;
        }

        let mut pending_targets = 0usize;
        let mut delivered_targets = 0usize;

        for idx in targets {
            if event.kind == "claim" && notify_targets.contains(&idx) {
                continue;
            }
            if should_skip_sender(idx, agent_names, &event.from) {
                continue;
            }
            pending_targets += 1;
            if !cooldown_ready(state, idx, config.cooldown, &event.kind) {
                continue;
            }
            let Some(pane) = panes.get(idx).and_then(|p| p.as_ref()) else {
                continue;
            };
            let line = format_injection(event);
            pane.inject_line(&line);
            if auto_wake_enabled() && is_delegate_notify(event) {
                let t0 = Instant::now();
                state.wake_after.push((idx, t0 + Duration::from_millis(700)));
                state.wake_after.push((idx, t0 + Duration::from_millis(2000)));
            }
            if let Some(slot) = state.last_sent.get_mut(idx) {
                *slot = Some(Instant::now());
            }
            if event.kind == "notify" {
                if let Some(target) = event.agent.as_deref() {
                    if let Some(key) = crate::mailbox_relay::dedupe_key_for_notify(
                        &event.message,
                        target,
                        &event.from,
                        event.time.as_deref(),
                    ) {
                        state.remember_dedupe(&key);
                    }
                }
            }
            delivered += 1;
            delivered_targets += 1;
        }

        if pending_targets == 0 || delivered_targets == pending_targets {
            state.cursor = event.line_no;
        }
    }
    delivered
}

fn should_skip_sender(index: usize, agent_names: &[String], from: &str) -> bool {
    agent_names
        .get(index)
        .is_some_and(|name| name.eq_ignore_ascii_case(from))
}

fn cooldown_ready(state: &RelayState, index: usize, cooldown: Duration, kind: &str) -> bool {
    if kind == "notify" {
        return true;
    }
    match state.last_sent.get(index).copied().flatten() {
        None => true,
        Some(t) => t.elapsed() >= cooldown,
    }
}

fn relay_targets(agent_names: &[String], event: &CoordEvent) -> Vec<usize> {
    match event.kind.as_str() {
        "notify" => agent_names
            .iter()
            .position(|n| event.agent.as_deref().is_some_and(|a| n == a))
            .into_iter()
            .collect(),
        "broadcast" => (0..agent_names.len()).collect(),
        "claim" => agent_names
            .iter()
            .position(|n| event.agent.as_deref().is_some_and(|a| n == a))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_notify() {
        let event = CoordEvent {
            line_no: 1,
            time: None,
            kind: "notify".into(),
            agent: Some("claude".into()),
            message: "请 review".into(),
            from: "mimo".into(),
            task: None,
            action: None,
        };
        assert_eq!(
            format_injection(&event),
            "[协调/mimo->claude] 请 review"
        );
    }

    #[test]
    fn skips_lead_briefing_relay() {
        let event = CoordEvent {
            line_no: 1,
            time: None,
            kind: "notify".into(),
            agent: Some("cursor".into()),
            message: "【协调规则·Lead 身份】你是 Lead".into(),
            from: "system".into(),
            task: None,
            action: None,
        };
        assert!(is_lead_briefing_notify(&event));
    }

    #[test]
    fn cursor_holds_when_pane_missing() {
        let events = vec![CoordEvent {
            line_no: 1,
            time: Some("2026-01-01T00:00:00Z".into()),
            kind: "notify".into(),
            agent: Some("codex".into()),
            message: "【委派·auth-api】请开始".into(),
            from: "cursor".into(),
            task: None,
            action: None,
        }];
        let names = vec!["cursor".into(), "codex".into()];
        let mut panes: Vec<Option<AgentPane>> = vec![None, None];
        let mut state = RelayState::new(std::path::Path::new("."), 2);
        let config = RelayConfig {
            enabled: true,
            cooldown: Duration::from_secs(12),
        };
        let delivered = dispatch(&config, &mut state, &names, &mut panes, &events);
        assert_eq!(delivered, 0);
        assert_eq!(state.cursor, 0, "cursor must not advance when PTY missing");
    }
}
