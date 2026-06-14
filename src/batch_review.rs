//! Batch code review after all plan tasks complete — gates merge-ready.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config;
use crate::delegation;
use crate::meta;
use crate::review_gate::pick_reviewer;
use crate::task_state;
use crate::terminal;

fn dispatched_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/batch_review_dispatched.jsonl")
}

pub fn batch_review_enabled() -> bool {
    std::env::var("AGENT_TUI_BATCH_REVIEW")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// Stable batch-review task id for a done-task set.
pub fn batch_review_task_id(done_tasks: &[String]) -> String {
    let prefix = crate::merge_ready::merge_batch_prefix().unwrap_or_else(|| "batch".into());
    let mut sorted = done_tasks.to_vec();
    sorted.sort();
    let short = sorted.join(",");
    let hash = short
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    format!("{prefix}-batch-review-{hash:08x}")
}

fn fingerprint(done_tasks: &[String]) -> String {
    let mut sorted = done_tasks.to_vec();
    sorted.sort();
    sorted.join(",")
}

fn already_dispatched(project_dir: &Path, fp: &str) -> bool {
    let path = dispatched_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    content.lines().any(|l| l.trim() == fp)
}

fn remember_dispatched(project_dir: &Path, fp: &str) -> Result<()> {
    let path = dispatched_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{fp}")?;
    Ok(())
}

pub fn batch_review_status(project_dir: &Path, task_id: &str) -> Option<String> {
    task_state::load_snapshots(project_dir)
        .get(task_id)
        .map(|s| s.status.clone())
}

pub fn batch_review_passed(project_dir: &Path, task_id: &str) -> bool {
    batch_review_status(project_dir, task_id).as_deref() == Some("done")
}

pub fn review_checklist(done_tasks: &[String]) -> String {
    let list = done_tasks.join(", ");
    format!(
        "【批次代码审查】子任务已全部完成并通过逐 task review：{list}。\n\
         请在本 worktree 执行：\n\
         1. `git diff` / `git log --oneline -10` 总览本批次改动\n\
         2. 检查：安全边界、回归风险、测试缺口、跨模块一致性\n\
         3. 对照 `.agents/STRENGTHS.md` 历史表现，标注需 Lead 改派的模式\n\
         4. 通过 → agent-report done；重大问题 → failed 并列出阻塞项\n\
         勿改代码，只审查；修复由 Lead 另派实现 task。"
    )
}

/// Delegate batch review to reviewer; returns true if newly dispatched.
pub fn dispatch_batch_review(
    project_dir: &Path,
    lead: &str,
    done_tasks: &[String],
) -> Result<bool> {
    if !batch_review_enabled() {
        return Ok(false);
    }
    let fp = fingerprint(done_tasks);
    let task_id = batch_review_task_id(done_tasks);
    if batch_review_passed(project_dir, &task_id) {
        return Ok(false);
    }
    if already_dispatched(project_dir, &fp) {
        // In flight or failed — do not re-dispatch until Lead resets or task completes.
        let st = batch_review_status(project_dir, &task_id).unwrap_or_default();
        if st != "done" && st != "failed" {
            return Ok(true);
        }
        if st == "failed" {
            return Ok(true);
        }
        return Ok(false);
    }
    let reviewer = pick_reviewer(project_dir, lead).unwrap_or_else(|| "mimo".into());
    let desc = review_checklist(done_tasks);
    delegation::delegate_task(project_dir, lead, &reviewer, &task_id, &desc)?;
    remember_dispatched(project_dir, &fp)?;
    terminal::log_message(
        project_dir,
        "info",
        &format!("batch review: 已委派 {reviewer}/{task_id}（{fp}）"),
    );
    meta::append_shared_line(
        project_dir,
        &format!("agent-tui → {reviewer} 批次审查「{task_id}」"),
    )?;
    Ok(true)
}

/// CLI / manual: review current merge-ready batch or explicit task list.
pub fn dispatch_review_for_project(project_dir: &Path, batch_prefix: Option<&str>) -> Result<String> {
    let agents = config::load_agents(project_dir)?;
    let lead = config::resolve_lead_agent(&agents);
    if let Some(prefix) = batch_prefix {
        std::env::set_var("AGENT_TUI_MERGE_BATCH", prefix);
    }
    let status = crate::merge_ready::evaluate(project_dir);
    if status.done_tasks.is_empty() {
        anyhow::bail!("没有可审查的已完成子任务（检查 AGENT_TUI_MERGE_BATCH 或先完成子任务）");
    }
    let task_id = batch_review_task_id(&status.done_tasks);
    let _ = dispatch_batch_review(project_dir, &lead, &status.done_tasks)?;
    Ok(task_id)
}

pub fn reset_dispatched(project_dir: &Path) -> Result<()> {
    let path = dispatched_path(project_dir);
    if path.is_file() {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_task_id() {
        let a = batch_review_task_id(&["b".into(), "a".into()]);
        let b = batch_review_task_id(&["a".into(), "b".into()]);
        assert_eq!(a, b);
        assert!(a.contains("batch-review"));
    }
}
