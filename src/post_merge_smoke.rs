//! Pre-PR smoke verification — reviewer runs sanity checks before pr-create / auto-pr.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::delegation;
use crate::merge_ready;
use crate::review_gate::pick_reviewer;

pub use crate::review_gate::is_smoke_task;
use crate::task_state;
use crate::terminal;

fn dispatched_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/post_merge_smoke_dispatched.jsonl")
}

pub fn post_merge_smoke_enabled() -> bool {
    std::env::var("AGENT_TUI_POST_MERGE_SMOKE")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}


pub fn smoke_task_id(done_tasks: &[String]) -> String {
    let prefix = merge_ready::merge_batch_prefix().unwrap_or_else(|| "batch".into());
    let fp = merge_ready::done_tasks_fingerprint(done_tasks);
    let hash = fp
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    format!("{prefix}-post-merge-smoke-{hash:08x}")
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

pub fn smoke_passed(project_dir: &Path, done_tasks: &[String]) -> bool {
    let id = smoke_task_id(done_tasks);
    task_state::load_snapshots(project_dir)
        .get(&id)
        .map(|s| s.status == "done")
        .unwrap_or(false)
}

/// True when smoke is disabled, or smoke task completed successfully.
pub fn smoke_gate_satisfied(project_dir: &Path, done_tasks: &[String]) -> bool {
    if !post_merge_smoke_enabled() {
        return true;
    }
    smoke_passed(project_dir, done_tasks)
}

pub fn smoke_checklist(done_tasks: &[String]) -> String {
    let list = done_tasks.join(", ");
    format!(
        "【合并前 smoke】批次 {list} 已通过 review，开 PR 前做快速验证：\n\
         1. 在 lead worktree 跑项目标准测试/构建（如 cargo test / npm test / pytest）\n\
         2. 确认无新增 lint/type 错误\n\
         3. 通过 → agent-report done；失败 → failed 并列出阻塞项\n\
         勿改功能代码，只验证；修复由 Lead 另派 task。"
    )
}

/// Delegate smoke to reviewer after merge-ready; returns true if newly dispatched.
pub fn dispatch_smoke(project_dir: &Path, lead: &str, done_tasks: &[String]) -> Result<bool> {
    if !post_merge_smoke_enabled() {
        return Ok(false);
    }
    let fp = merge_ready::done_tasks_fingerprint(done_tasks);
    let task_id = smoke_task_id(done_tasks);
    if smoke_passed(project_dir, done_tasks) {
        return Ok(false);
    }
    if already_dispatched(project_dir, &fp) {
        let snap = task_state::load_snapshots(project_dir);
        let st = snap
            .get(&task_id)
            .map(|s| s.status.as_str())
            .unwrap_or("missing");
        return Ok(st != "done" && st != "failed");
    }
    let reviewer = pick_reviewer(project_dir, lead).unwrap_or_else(|| "mimo".into());
    let desc = smoke_checklist(done_tasks);
    delegation::delegate_task(project_dir, lead, &reviewer, &task_id, &desc)?;
    remember_dispatched(project_dir, &fp)?;
    terminal::log_message(
        project_dir,
        "info",
        &format!("post-merge smoke: 已委派 {reviewer}/{task_id}"),
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("smoke-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        dir
    }

    #[test]
    fn smoke_task_id_stable() {
        let a = smoke_task_id(&["b".into(), "a".into()]);
        let b = smoke_task_id(&["a".into(), "b".into()]);
        assert_eq!(a, b);
        assert!(is_smoke_task(&a));
    }

    #[test]
    fn gate_open_when_disabled() {
        std::env::set_var("AGENT_TUI_POST_MERGE_SMOKE", "0");
        let dir = fresh_dir("gate");
        assert!(smoke_gate_satisfied(&dir, &["x".into()]));
        std::env::remove_var("AGENT_TUI_POST_MERGE_SMOKE");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dispatch_smoke_records_task() {
        let dir = fresh_dir("disp");
        crate::project_init::init_project(
            &dir,
            &crate::project_init::InitOptions {
                force: true,
                link_worktrees: false,
                sync_lead: false,
                minimal: true,
            },
        )
        .unwrap();
        let lead = "cursor";
        let done = vec!["feat-a".into()];
        std::env::set_var("AGENT_TUI_POST_MERGE_SMOKE", "1");
        let ok = dispatch_smoke(&dir, lead, &done).unwrap();
        std::env::remove_var("AGENT_TUI_POST_MERGE_SMOKE");
        assert!(ok);
        let id = smoke_task_id(&done);
        assert!(task_state::load_snapshots(&dir).get(&id).is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
