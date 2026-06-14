//! Task lifecycle state machine (transition log → current status per task).

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::meta::inbox_timestamp_iso;

fn state_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/task_state.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTransition {
    pub time: String,
    pub task: String,
    pub status: String,
    #[serde(default)]
    pub worker: Option<String>,
    #[serde(default)]
    pub lead: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    pub task: String,
    pub status: String,
    pub worker: Option<String>,
    pub lead: Option<String>,
    pub updated: String,
    pub summary: Option<String>,
}

pub fn record_transition(
    project_dir: &Path,
    task: &str,
    status: &str,
    worker: Option<&str>,
    lead: Option<&str>,
    summary: Option<&str>,
    source: &str,
) -> Result<()> {
    let path = state_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let rec = TaskTransition {
        time: inbox_timestamp_iso(),
        task: task.to_string(),
        status: status.to_string(),
        worker: worker.map(str::to_string),
        lead: lead.map(str::to_string),
        summary: summary.map(str::to_string),
        source: source.to_string(),
    };
    let line = serde_json::to_string(&rec)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

pub fn on_plan_pending(project_dir: &Path, lead: &str, worker: &str, task: &str) -> Result<()> {
    record_transition(
        project_dir,
        task,
        "pending",
        Some(worker),
        Some(lead),
        None,
        "plan_dag",
    )
}

pub fn on_delegate(
    project_dir: &Path,
    lead: &str,
    worker: &str,
    task: &str,
    description: &str,
) -> Result<()> {
    record_transition(
        project_dir,
        task,
        "delegated",
        Some(worker),
        Some(lead),
        Some(description),
        "delegate",
    )
}

pub fn on_report(
    project_dir: &Path,
    reporter: &str,
    lead: &str,
    task: &str,
    status: &str,
    summary: &str,
) -> Result<()> {
    let status = match status {
        "done" => "done",
        "failed" => "failed",
        "blocked" => "blocked",
        other => other,
    };
    record_transition(
        project_dir,
        task,
        status,
        Some(reporter),
        Some(lead),
        Some(summary),
        "report",
    )
}

pub fn load_snapshots(project_dir: &Path) -> HashMap<String, TaskSnapshot> {
    let path = state_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<TaskTransition>(line) else {
            continue;
        };
        map.insert(
            rec.task.clone(),
            TaskSnapshot {
                task: rec.task,
                status: rec.status,
                worker: rec.worker,
                lead: rec.lead,
                updated: rec.time,
                summary: rec.summary,
            },
        );
    }
    map
}

pub fn tasks_by_status<'a>(
    snapshots: &'a HashMap<String, TaskSnapshot>,
    status: &str,
) -> Vec<&'a TaskSnapshot> {
    let mut out: Vec<_> = snapshots
        .values()
        .filter(|s| s.status == status)
        .collect();
    out.sort_by(|a, b| a.task.cmp(&b.task));
    out
}

pub fn format_status_line(s: &TaskSnapshot) -> String {
    let worker = s.worker.as_deref().unwrap_or("-");
    let icon = match s.status.as_str() {
        "done" => "✓",
        "failed" => "✗",
        "blocked" => "⏸",
        "awaiting_review" => "👁",
        "review_failed" => "⊘",
        "pending" => "⏳",
        "delegated" => "→",
        _ => "·",
    };
    format!("  {icon} {worker}/{task} ({status})", task = s.task, status = s.status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_status_wins() {
        let dir = std::env::temp_dir().join(format!(
            "agent-tui-state-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        on_delegate(&dir, "cursor", "codex", "t1", "work").unwrap();
        on_report(&dir, "codex", "cursor", "t1", "done", "ok").unwrap();
        let snap = load_snapshots(&dir);
        assert_eq!(snap.get("t1").map(|s| s.status.as_str()), Some("done"));
        let _ = fs::remove_dir_all(&dir);
    }
}
