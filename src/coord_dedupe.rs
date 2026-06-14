//! Persistent dedupe fingerprints so TUI restarts do not re-dispatch plans/reports/relay.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

const PLANS: &str = "shared/dispatched_plans.jsonl";
const REPORTS: &str = "shared/processed_reports.jsonl";
const RELAY: &str = "shared/relay_dedupe.jsonl";

fn path(project_dir: &Path, rel: &str) -> std::path::PathBuf {
    project_dir.join(".agents").join(rel)
}

fn load_keys(project_dir: &Path, rel: &str) -> HashSet<String> {
    let path = path(project_dir, rel);
    let Ok(content) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn append_key(project_dir: &Path, rel: &str, key: &str) -> std::io::Result<()> {
    let path = path(project_dir, rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{key}")?;
    Ok(())
}

pub fn load_plan_fingerprints(project_dir: &Path) -> HashSet<String> {
    load_keys(project_dir, PLANS)
}

pub fn remember_plan_fingerprint(project_dir: &Path, fp: &str) {
    let _ = append_key(project_dir, PLANS, fp);
}

pub fn load_report_fingerprints(project_dir: &Path) -> HashSet<String> {
    load_keys(project_dir, REPORTS)
}

/// Stable report dedupe (reporter + task + status; summary excluded for restart safety).
pub fn report_key(reporter: &str, task: &str, status: &str) -> String {
    format!("{}:{}:{}", reporter, task, status)
}

pub fn remember_report_fingerprint(project_dir: &Path, key: &str) {
    let _ = append_key(project_dir, REPORTS, key);
}

pub fn load_relay_dedupe(project_dir: &Path) -> HashSet<String> {
    load_keys(project_dir, RELAY)
}

pub fn remember_relay_dedupe(project_dir: &Path, key: &str) {
    let _ = append_key(project_dir, RELAY, key);
}

const RELAY_CURSOR: &str = "shared/relay_cursor.json";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RelayCursorState {
    pub events_cursor: usize,
    pub mailbox_line: usize,
    pub plan_inbox_line: usize,
    /// Initial Lead briefing already delivered (skip on TUI restart for same lead).
    #[serde(default)]
    pub initial_briefing_sent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_briefing_lead: Option<String>,
}

pub fn initial_briefing_already_sent(project_dir: &Path, lead: &str) -> bool {
    let s = load_relay_cursor(project_dir);
    s.initial_briefing_sent && s.initial_briefing_lead.as_deref() == Some(lead)
}

pub fn mark_initial_briefing_sent(project_dir: &Path, lead: &str) {
    let mut s = load_relay_cursor(project_dir);
    s.initial_briefing_sent = true;
    s.initial_briefing_lead = Some(lead.to_string());
    let _ = save_relay_cursor(project_dir, &s);
}

/// After `!sync-lead` — allow Lead PTY to receive a fresh briefing on next inject.
pub fn clear_initial_briefing(project_dir: &Path) {
    let mut s = load_relay_cursor(project_dir);
    s.initial_briefing_sent = false;
    s.initial_briefing_lead = None;
    let _ = save_relay_cursor(project_dir, &s);
}

pub fn update_relay_cursor_fields(
    project_dir: &Path,
    events_cursor: usize,
    mailbox_line: usize,
    plan_inbox_line: usize,
) {
    let mut s = load_relay_cursor(project_dir);
    s.events_cursor = events_cursor;
    s.mailbox_line = mailbox_line;
    s.plan_inbox_line = plan_inbox_line;
    let _ = save_relay_cursor(project_dir, &s);
}

pub fn load_relay_cursor(project_dir: &Path) -> RelayCursorState {
    let path = path(project_dir, RELAY_CURSOR);
    let Ok(content) = fs::read_to_string(path) else {
        return RelayCursorState::default();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_relay_cursor(project_dir: &Path, state: &RelayCursorState) -> std::io::Result<()> {
    let path = path(project_dir, RELAY_CURSOR);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".into());
    fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn persists_and_reloads_plan_fp() {
        let dir = env::temp_dir().join(format!("coord-dedupe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        remember_plan_fingerprint(&dir, "codex:auth-api");
        let loaded = load_plan_fingerprints(&dir);
        assert!(loaded.contains("codex:auth-api"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn persists_initial_briefing_flag() {
        let dir = env::temp_dir().join(format!("coord-briefing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        assert!(!initial_briefing_already_sent(&dir, "cursor"));
        mark_initial_briefing_sent(&dir, "cursor");
        assert!(initial_briefing_already_sent(&dir, "cursor"));
        assert!(!initial_briefing_already_sent(&dir, "claude"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clears_initial_briefing_flag() {
        let dir = env::temp_dir().join(format!("coord-briefing-clear-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        mark_initial_briefing_sent(&dir, "cursor");
        assert!(initial_briefing_already_sent(&dir, "cursor"));
        clear_initial_briefing(&dir);
        assert!(!initial_briefing_already_sent(&dir, "cursor"));
        let _ = fs::remove_dir_all(&dir);
    }
}
