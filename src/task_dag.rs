//! Task dependency scheduling for agent-plan items (`depends_on`).

use std::collections::{HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::lead_watch::PlanItem;
use crate::meta::inbox_timestamp_iso;

fn completed_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/completed_tasks.jsonl")
}

fn pending_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/pending_plans.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompletedRecord {
    task: String,
    time: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingRecord {
    worker: String,
    task: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    depends_on: Vec<String>,
}

pub fn load_completed_tasks(project_dir: &Path) -> HashSet<String> {
    let path = completed_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<CompletedRecord>(line.trim()).ok())
        .map(|r| r.task)
        .collect()
}

pub fn mark_task_completed(project_dir: &Path, task: &str) -> Result<()> {
    let path = completed_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let rec = CompletedRecord {
        task: task.to_string(),
        time: inbox_timestamp_iso(),
    };
    let line = serde_json::to_string(&rec)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

pub fn load_pending_plans(project_dir: &Path) -> Vec<PlanItem> {
    let path = pending_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let rec: PendingRecord = serde_json::from_str(line.trim()).ok()?;
            Some(PlanItem {
                worker: rec.worker,
                task: rec.task,
                description: rec.description,
                depends_on: rec.depends_on,
            })
        })
        .collect()
}

pub fn save_pending_plans(project_dir: &Path, items: &[PlanItem]) -> Result<()> {
    let path = pending_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut body = String::new();
    for item in items {
        let rec = PendingRecord {
            worker: item.worker.clone(),
            task: item.task.clone(),
            description: item.description.clone(),
            depends_on: item.depends_on.clone(),
        };
        body.push_str(&serde_json::to_string(&rec)?);
        body.push('\n');
    }
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn plan_key(item: &PlanItem) -> String {
    format!("{}:{}", item.worker.to_lowercase(), item.task)
}

pub fn deps_satisfied(item: &PlanItem, completed: &HashSet<String>) -> bool {
    if item.depends_on.is_empty() {
        return true;
    }
    let done_lower: HashSet<String> = completed.iter().map(|t| t.to_lowercase()).collect();
    item.depends_on.iter().all(|d| {
        let d = d.trim();
        !d.is_empty() && (completed.contains(d) || done_lower.contains(&d.to_lowercase()))
    })
}

fn normalize_completed(completed: &HashSet<String>) -> HashSet<String> {
    completed
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Split plans into ready vs blocked; merge with persisted pending queue.
pub fn schedule_plans(
    project_dir: &Path,
    new_items: Vec<PlanItem>,
    completed: &HashSet<String>,
) -> Result<(Vec<PlanItem>, Vec<PlanItem>)> {
    let completed = normalize_completed(completed);
    let mut pending: VecDeque<PlanItem> = load_pending_plans(project_dir).into();
    let mut seen: HashSet<String> = pending.iter().map(plan_key).collect();

    for item in new_items {
        let key = plan_key(&item);
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        pending.push_back(item);
    }

    let mut ready = Vec::new();
    let mut still_pending = Vec::new();
    let mut progress = true;
    while progress {
        progress = false;
        let mut next = VecDeque::new();
        while let Some(item) = pending.pop_front() {
            if deps_satisfied(&item, &completed) {
                ready.push(item);
                progress = true;
            } else {
                next.push_back(item);
            }
        }
        pending = next;
    }
    still_pending.extend(pending);
    save_pending_plans(project_dir, &still_pending)?;
    Ok((ready, still_pending))
}

/// After a task completes, promote pending plans whose deps are now satisfied.
pub fn flush_pending_after_complete(
    project_dir: &Path,
    completed: &HashSet<String>,
) -> Result<Vec<PlanItem>> {
    let (ready, _) = schedule_plans(project_dir, Vec::new(), completed)?;
    Ok(ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deps_block_until_complete() {
        let mut done = HashSet::new();
        let item = PlanItem {
            worker: "kimi".into(),
            task: "ui".into(),
            description: String::new(),
            depends_on: vec!["api".into()],
        };
        assert!(!deps_satisfied(&item, &done));
        done.insert("api".into());
        assert!(deps_satisfied(&item, &done));
    }
}
