//! Task board summary for CLI / status strip.

use std::path::Path;

use anyhow::{Context, Result};

use crate::claims;
use crate::config;
use crate::dead_letter;
use crate::task_dag;
use crate::task_state;

pub fn format_task_board(project_dir: &Path) -> Result<String> {
    let agents = config::load_agents(project_dir).context("load agents")?;
    let lead = config::resolve_lead_agent(&agents);
    let snapshot = claims::load_claims_snapshot(project_dir);
    let completed = task_dag::load_completed_tasks(project_dir);
    let pending = task_dag::load_pending_plans(project_dir);
    let states = task_state::load_snapshots(project_dir);

    let mut lines = vec![
        format!("主 Agent: {lead}"),
        String::from("── 状态机（task_state） ──"),
    ];
    let mut any_state = false;
    for status in [
        "delegated",
        "pending",
        "awaiting_review",
        "review_failed",
        "blocked",
        "failed",
        "done",
    ] {
        let group = task_state::tasks_by_status(&states, status);
        if group.is_empty() {
            continue;
        }
        any_state = true;
        lines.push(format!("  [{status}]"));
        for s in group.iter().take(8) {
            lines.push(task_state::format_status_line(s));
        }
    }
    if !any_state {
        lines.push("  (无)".into());
    }

    lines.push(String::from("── 活跃认领 ──"));
    let mut any_active = false;
    for spec in &agents {
        if let Some(tasks) = snapshot.agent_tasks.get(&spec.name) {
            if tasks.is_empty() {
                continue;
            }
            any_active = true;
            lines.push(format!("  {}: {}", spec.name, tasks.join(", ")));
        }
    }
    if !any_active {
        lines.push("  (无)".into());
    }

    lines.push(String::from("── 已完成（DAG） ──"));
    if completed.is_empty() {
        lines.push("  (无)".into());
    } else {
        for t in completed.iter().take(12) {
            lines.push(format!("  ✓ {t}"));
        }
    }

    lines.push(String::from("── 等待依赖 ──"));
    if pending.is_empty() {
        lines.push("  (无)".into());
    } else {
        for p in &pending {
            let deps = if p.depends_on.is_empty() {
                "-".to_string()
            } else {
                p.depends_on.join(",")
            };
            lines.push(format!(
                "  {} → {} (deps: {deps})",
                p.worker, p.task
            ));
        }
    }

    if !snapshot.conflict_tasks.is_empty() {
        lines.push(String::from("── ⚡ 冲突 ──"));
        for t in &snapshot.conflict_tasks {
            lines.push(format!("  {t}"));
        }
    }

    lines.push(String::from("── 死信 / 待重试 ──"));
    let dead = dead_letter::load_dead_letters(project_dir, 8);
    if dead.is_empty() {
        lines.push("  (无)".into());
    } else {
        for r in &dead {
            lines.push(dead_letter::format_dead_letter(r));
        }
    }

    Ok(lines.join("\n"))
}
