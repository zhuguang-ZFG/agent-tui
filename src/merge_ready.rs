//! Merge-ready detection: all implementation tasks reviewed and terminal → notify Lead.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::meta;
use crate::review_gate::is_meta_task;
use crate::task_state;
use crate::terminal;

fn notified_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/merge_ready_notified.jsonl")
}

pub fn merge_ready_enabled() -> bool {
    std::env::var("AGENT_TUI_MERGE_READY")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

#[derive(Debug, Clone)]
pub struct MergeReadyStatus {
    pub ready: bool,
    pub done_tasks: Vec<String>,
    pub blocking: Vec<String>,
}

pub fn merge_batch_prefix() -> Option<String> {
    std::env::var("AGENT_TUI_MERGE_BATCH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// True when every tracked implementation task is `done` and none are in-flight/blocked.
pub fn evaluate(project_dir: &Path) -> MergeReadyStatus {
    let snap = task_state::load_snapshots(project_dir);
    let batch = merge_batch_prefix();
    if snap.is_empty() {
        return MergeReadyStatus {
            ready: false,
            done_tasks: vec![],
            blocking: vec![],
        };
    }

    let mut impl_tasks: BTreeSet<String> = BTreeSet::new();
    for (task, s) in &snap {
        if is_meta_task(task) || crate::verify_cleanup::is_verify_artifact_task(task) {
            continue;
        }
        if let Some(ref prefix) = batch {
            if !task.starts_with(prefix) {
                continue;
            }
        }
        // Only tasks that entered the state machine (exclude stray empty)
        if matches!(
            s.status.as_str(),
            "delegated" | "pending" | "awaiting_review" | "review_failed" | "blocked" | "failed" | "done"
        ) {
            impl_tasks.insert(task.clone());
        }
    }

    if impl_tasks.is_empty() {
        return MergeReadyStatus {
            ready: false,
            done_tasks: vec![],
            blocking: vec![],
        };
    }

    let mut done_tasks = Vec::new();
    let mut blocking = Vec::new();
    for task in &impl_tasks {
        match snap.get(task).map(|s| s.status.as_str()) {
            Some("done") => done_tasks.push(task.clone()),
            Some(status) => blocking.push(format!("{task}:{status}")),
            None => blocking.push(format!("{task}:missing")),
        }
    }

    let ready = blocking.is_empty() && !done_tasks.is_empty();
    MergeReadyStatus {
        ready,
        done_tasks,
        blocking,
    }
}

fn batch_fingerprint(tasks: &[String]) -> String {
    let mut sorted = tasks.to_vec();
    sorted.sort();
    sorted.join(",")
}

/// Stable fingerprint for a done-task set (merge-ready / smoke / auto-pr dedupe).
pub fn done_tasks_fingerprint(tasks: &[String]) -> String {
    batch_fingerprint(tasks)
}

fn already_notified(project_dir: &Path, fingerprint: &str) -> bool {
    let path = notified_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    content.lines().any(|l| l.trim() == fingerprint)
}

fn remember_notified(project_dir: &Path, fingerprint: &str) -> Result<()> {
    let path = notified_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{fingerprint}")?;
    Ok(())
}

/// True when merge-ready notification was already sent for this done-task set.
pub fn was_notified_for_done_tasks(project_dir: &Path, done_tasks: &[String]) -> bool {
    already_notified(project_dir, &batch_fingerprint(done_tasks))
}

/// Notify Lead once per unique done-task set when merge-ready.
pub fn notify_lead_if_ready(project_dir: &Path, lead: &str) -> Result<bool> {
    if !merge_ready_enabled() {
        return Ok(false);
    }
    let status = evaluate(project_dir);
    if !status.ready {
        return Ok(false);
    }
    let fp = batch_fingerprint(&status.done_tasks);
    if already_notified(project_dir, &fp) {
        return Ok(false);
    }

    let batch_task = crate::batch_review::batch_review_task_id(&status.done_tasks);
    if crate::batch_review::batch_review_enabled() {
        if crate::batch_review::dispatch_batch_review(project_dir, lead, &status.done_tasks)? {
            terminal::log_message(
                project_dir,
                "info",
                &format!("merge-ready: 等待批次审查 {batch_task} 完成"),
            );
            return Ok(false);
        }
        if !crate::batch_review::batch_review_passed(project_dir, &batch_task) {
            let st = crate::batch_review::batch_review_status(project_dir, &batch_task)
                .unwrap_or_else(|| "missing".into());
            if st == "failed" {
                let _ = crate::batch_review::notify_failed_once(project_dir, lead, &batch_task)?;
            }
            return Ok(false);
        }
    }

    let task_list = status.done_tasks.join(", ");
    let body = format!(
        "【merge-ready】全部子任务已 review 通过：{task_list}。\n\
         ▶ Lead 行动：确认 diff → smoke 验证通过后 `agent-tui pr-create` / `agent-tui merge --all`（或设 AGENT_TUI_AUTO_PR=1 自动开 PR）。\n\
         勿问用户是否合并 — 默认进入合并准备。"
    );
    meta::notify_agent_from(project_dir, lead, &body, "agent-tui")?;
    meta::append_shared_line(
        project_dir,
        &format!("agent-tui → {lead} merge-ready（{task_list}）"),
    )?;
    remember_notified(project_dir, &fp)?;
    if let Ok(Some(msg)) = crate::project_map::maybe_refresh(project_dir) {
        terminal::log_message(project_dir, "info", &msg);
    }
    terminal::log_message(
        project_dir,
        "info",
        &format!("merge-ready: 已通知 {lead}（{task_list}）"),
    );
    let _ = crate::delegation_stats::evolve_project(project_dir);

    if crate::post_merge_smoke::post_merge_smoke_enabled() {
        let _ = crate::post_merge_smoke::dispatch_smoke(project_dir, lead, &status.done_tasks)?;
    } else {
        let _ = crate::auto_pr::maybe_create(project_dir, &status.done_tasks)?;
    }

    Ok(true)
}

/// Clear notified fingerprints (testing / new sprint).
pub fn reset_notified(project_dir: &Path) -> Result<()> {
    let path = notified_path(project_dir);
    if path.is_file() {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_state;

    #[test]
    fn ready_when_all_done() {
        let dir = std::env::temp_dir().join(format!("merge-ready-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        task_state::on_report(&dir, "codex", "cursor", "a", "done", "ok").unwrap();
        let s = evaluate(&dir);
        assert!(s.ready);
        assert_eq!(s.done_tasks, vec!["a".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_ready_with_awaiting_review() {
        let dir = std::env::temp_dir().join(format!("merge-ready-b-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        task_state::on_report(&dir, "codex", "cursor", "a", "awaiting_review", "x").unwrap();
        let s = evaluate(&dir);
        assert!(!s.ready);
        let _ = fs::remove_dir_all(&dir);
    }
}
