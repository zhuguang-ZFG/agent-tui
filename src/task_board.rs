//! Task board summary for CLI / Ctrl+T overlay.

use std::path::Path;

use anyhow::Result;

use crate::batch_group;
use crate::claims;
use crate::dead_letter;
use crate::task_dag;

pub fn format_task_board(project_dir: &Path) -> Result<String> {
    let _ = crate::routing::ensure_routing_template(project_dir);
    let collapsed = std::collections::BTreeSet::new();
    let mut lines = batch_group::format_grouped_board(project_dir, &collapsed);

    let snapshot = claims::load_claims_snapshot(project_dir);
    let pending = task_dag::load_pending_plans(project_dir);

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

pub fn format_task_board_collapsed(
    project_dir: &Path,
    collapsed: &std::collections::BTreeSet<String>,
) -> Result<String> {
    let _ = crate::routing::ensure_routing_template(project_dir);
    let mut lines = batch_group::format_grouped_board(project_dir, collapsed);
    let pending = task_dag::load_pending_plans(project_dir);
    if !pending.is_empty() {
        lines.push(String::from("── 等待依赖 ──"));
        for p in &pending {
            lines.push(format!("  {} → {}", p.worker, p.task));
        }
    }
    Ok(lines.join("\n"))
}
