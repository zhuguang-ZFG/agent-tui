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
}
