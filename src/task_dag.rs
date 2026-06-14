//! Task dependency scheduling for agent-plan items (`depends_on`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
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

/// Return one cycle path if `depends_on` forms a loop (e.g. A→B→A).
pub fn find_dependency_cycle(plans: &[PlanItem]) -> Option<Vec<String>> {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for p in plans {
        adj.entry(p.task.clone()).or_default();
        for d in &p.depends_on {
            let d = d.trim();
            if d.is_empty() {
                continue;
            }
            adj.entry(p.task.clone()).or_default().push(d.to_string());
            adj.entry(d.to_string()).or_default();
        }
    }

    #[derive(Copy, Clone, Eq, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let mut color: HashMap<String, Color> = adj.keys().map(|k| (k.clone(), Color::White)).collect();
    let mut stack: Vec<String> = Vec::new();

    fn dfs(
        node: &str,
        adj: &HashMap<String, Vec<String>>,
        color: &mut HashMap<String, Color>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        color.insert(node.to_string(), Color::Gray);
        stack.push(node.to_string());
        for dep in adj.get(node).into_iter().flatten() {
            match color.get(dep.as_str()).copied().unwrap_or(Color::White) {
                Color::Gray => {
                    if let Some(pos) = stack.iter().position(|t| t == dep) {
                        let mut cycle = stack[pos..].to_vec();
                        cycle.push(dep.clone());
                        return Some(cycle);
                    }
                }
                Color::White => {
                    if let Some(c) = dfs(dep, adj, color, stack) {
                        return Some(c);
                    }
                }
                Color::Black => {}
            }
        }
        stack.pop();
        color.insert(node.to_string(), Color::Black);
        None
    }

    for node in adj.keys().cloned().collect::<Vec<_>>() {
        if color.get(&node).copied().unwrap_or(Color::White) == Color::White {
            if let Some(c) = dfs(&node, &adj, &mut color, &mut stack) {
                return Some(c);
            }
        }
    }
    None
}

pub fn validate_no_dependency_cycles(plans: &[PlanItem]) -> Result<()> {
    if let Some(cycle) = find_dependency_cycle(plans) {
        bail!("dependency cycle detected: {}", cycle.join(" → "));
    }
    Ok(())
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

    let merged: Vec<PlanItem> = pending.iter().cloned().collect();
    validate_no_dependency_cycles(&merged)?;
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

    fn item(worker: &str, task: &str, deps: &[&str]) -> PlanItem {
        PlanItem {
            worker: worker.into(),
            task: task.into(),
            description: String::new(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agent-tui-dag-{}-{}",
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        dir
    }

    #[test]
    fn deps_block_until_complete() {
        let mut done = HashSet::new();
        let item = item("kimi", "ui", &["api"]);
        assert!(!deps_satisfied(&item, &done));
        done.insert("api".into());
        assert!(deps_satisfied(&item, &done));
    }

    #[test]
    fn empty_deps_is_always_ready() {
        let i = item("kimi", "solo", &[]);
        assert!(deps_satisfied(&i, &HashSet::new()));
    }

    #[test]
    fn deps_match_case_insensitively() {
        let mut done = HashSet::new();
        done.insert("API".into());
        let i = item("kimi", "ui", &["api"]);
        assert!(deps_satisfied(&i, &done));

        let i2 = item("kimi", "ui", &["Api"]);
        assert!(deps_satisfied(&i2, &done));
    }

    #[test]
    fn plan_key_lowercases_worker_only() {
        let i = item("Codex", "Build-UI", &[]);
        assert_eq!(plan_key(&i), "codex:Build-UI");
    }

    #[test]
    fn detects_simple_cycle() {
        let plans = vec![item("codex", "a", &["b"]), item("kimi", "b", &["a"])];
        assert!(find_dependency_cycle(&plans).is_some());
        assert!(validate_no_dependency_cycles(&plans).is_err());
    }

    #[test]
    fn detects_self_cycle() {
        let plans = vec![item("codex", "a", &["a"])];
        assert!(find_dependency_cycle(&plans).is_some());
    }

    #[test]
    fn no_cycle_in_three_hop_chain() {
        let plans = vec![
            item("codex", "c", &["b"]),
            item("kimi", "b", &["a"]),
            item("mimo", "a", &[]),
        ];
        assert!(find_dependency_cycle(&plans).is_none());
        assert!(validate_no_dependency_cycles(&plans).is_ok());
    }

    #[test]
    fn pending_plans_roundtrip_through_disk() {
        let dir = temp_dir("roundtrip");
        let plans = vec![
            item("codex", "t1", &[]),
            item("kimi", "t2", &["t1"]),
        ];
        save_pending_plans(&dir, &plans).unwrap();
        let loaded = load_pending_plans(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].task, "t1");
        assert_eq!(loaded[1].depends_on, vec!["t1".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn schedule_dedupes_by_key_and_persists_blocked() {
        let dir = temp_dir("schedule");
        let completed = HashSet::new();
        let incoming = vec![
            item("codex", "ui", &["api"]),
            item("Codex", "ui", &["api"]), // dup (worker lowercased)
            item("kimi", "api", &[]),
        ];
        let (ready, still) = schedule_plans(&dir, incoming, &completed).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].task, "api");
        assert_eq!(still.len(), 1);
        assert_eq!(still[0].task, "ui");
        // persisted
        let reloaded = load_pending_plans(&dir);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].task, "ui");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flush_promotes_blocked_after_dep_completes() {
        let dir = temp_dir("flush");
        let plans = vec![item("codex", "ui", &["api"]), item("kimi", "api", &[])];
        let (ready, _) = schedule_plans(&dir, plans, &HashSet::new()).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].task, "api");

        let mut completed = HashSet::new();
        completed.insert("api".into());
        let promoted = flush_pending_after_complete(&dir, &completed).unwrap();
        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0].task, "ui");
        let _ = fs::remove_dir_all(&dir);
    }
}
