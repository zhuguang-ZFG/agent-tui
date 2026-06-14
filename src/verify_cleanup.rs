//! Detect and remove headless `verify-loop` artifacts from real project dirs.

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

const VERIFY_PREFIXES: &[&str] = &[
    "loop-verify-",
    "retry-verify-",
    "dag-dep-",
    "dag-ui-",
    "follow-dep-",
    "follow-next-",
    "tx-follow-dep-",
    "tx-follow-next-",
    "dedupe-plan-",
    "cycle-a-",
    "cycle-b-",
    "blocked-esc-",
];

#[derive(Debug, Clone, Default)]
pub struct CleanupReport {
    pub tasks_removed: usize,
    pub files_touched: Vec<String>,
}

pub fn is_verify_artifact_task(task: &str) -> bool {
    if task.ends_with("-unblock") {
        let parent = task.strip_suffix("-unblock").unwrap_or(task);
        return is_verify_artifact_task(parent);
    }
    if task.ends_with("-review") {
        let parent = task.strip_suffix("-review").unwrap_or(task);
        if is_verify_artifact_task(parent) {
            return true;
        }
    }
    VERIFY_PREFIXES.iter().any(|p| task.starts_with(p))
}

fn filter_jsonl_by_task(path: &Path, removed: &mut usize) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let content = fs::read_to_string(path)?;
    let mut kept = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let task = v
                .get("task")
                .and_then(|t| t.as_str())
                .or_else(|| v.get("parent_task").and_then(|t| t.as_str()));
            if let Some(task) = task {
                if is_verify_artifact_task(task) {
                    *removed += 1;
                    continue;
                }
            }
        }
        kept.push(line);
    }
    let new_body = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    if new_body != content {
        fs::write(path, new_body)?;
        return Ok(true);
    }
    Ok(false)
}

fn filter_plain_lines(path: &Path, removed: &mut usize) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let content = fs::read_to_string(path)?;
    let mut kept = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if is_verify_artifact_task(t) {
            *removed += 1;
            continue;
        }
        kept.push(t);
    }
    let new_body = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    if new_body != content {
        fs::write(path, new_body)?;
        return Ok(true);
    }
    Ok(false)
}

#[derive(Deserialize)]
struct FollowupLine {
    task: String,
}

fn filter_lead_followup(path: &Path, removed: &mut usize) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let content = fs::read_to_string(path)?;
    let mut kept = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<FollowupLine>(line) {
            if is_verify_artifact_task(&rec.task) {
                *removed += 1;
                continue;
            }
        }
        kept.push(line);
    }
    let new_body = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    if new_body != content {
        fs::write(path, new_body)?;
        return Ok(true);
    }
    Ok(false)
}

/// Strip verify-loop tasks from shared state so `next` reflects real work only.
pub fn cleanup_verify_artifacts(project_dir: &Path) -> Result<CleanupReport> {
    let shared = project_dir.join(".agents/shared");
    let mut report = CleanupReport::default();

    let jsonl_targets = [
        "task_state.jsonl",
        "mailbox.jsonl",
        "dead_letter.jsonl",
        "delegation_outcomes.jsonl",
        "dispatched_plans.jsonl",
    ];
    for name in jsonl_targets {
        let path = shared.join(name);
        if filter_jsonl_by_task(&path, &mut report.tasks_removed)? {
            report.files_touched.push(name.into());
        }
    }

    if filter_plain_lines(&shared.join("completed_tasks.jsonl"), &mut report.tasks_removed)? {
        report.files_touched.push("completed_tasks.jsonl".into());
    }
    if filter_lead_followup(&shared.join("lead_followup.jsonl"), &mut report.tasks_removed)? {
        report.files_touched.push("lead_followup.jsonl".into());
    }

    Ok(report)
}

pub fn format_cleanup_report(report: &CleanupReport) -> String {
    if report.tasks_removed == 0 {
        return "未发现 verify-loop 残留任务。".into();
    }
    format!(
        "已清理 {} 条 verify-loop 残留（{}）",
        report.tasks_removed,
        report.files_touched.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_verify_prefixes() {
        assert!(is_verify_artifact_task("loop-verify-123"));
        assert!(is_verify_artifact_task("blocked-esc-99-review"));
        assert!(is_verify_artifact_task("blocked-esc-99-unblock"));
        assert!(!is_verify_artifact_task("feat-auth"));
    }

    #[test]
    fn cleanup_removes_verify_tasks_from_state() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("verify-cleanup-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".agents/shared"))?;
        let state = dir.join(".agents/shared/task_state.jsonl");
        let mut f = std::fs::File::create(&state)?;
        writeln!(
            f,
            r#"{{"task":"loop-verify-abc","status":"blocked"}}"#
        )?;
        writeln!(
            f,
            r#"{{"task":"real-feature","status":"done"}}"#
        )?;

        let report = cleanup_verify_artifacts(&dir)?;
        assert_eq!(report.tasks_removed, 1);
        let left = std::fs::read_to_string(state)?;
        assert!(left.contains("real-feature"));
        assert!(!left.contains("loop-verify"));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
