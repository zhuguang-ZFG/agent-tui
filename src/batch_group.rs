//! Group tasks by sprint/batch prefix for the task board (ZCode-style workspace).

use std::collections::BTreeMap;
use std::path::Path;

use crate::agent_strengths;
use crate::config::{self, AgentSpec};
use crate::merge_ready;
use crate::review_gate::is_meta_task;
use crate::task_state::{self, TaskSnapshot};
use crate::verify_cleanup::is_verify_artifact_task;

#[derive(Debug, Clone)]
pub struct BatchSummary {
    pub id: String,
    pub tasks: Vec<TaskRow>,
    pub done: usize,
    pub active: usize,
    pub blocked: usize,
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub task: String,
    pub status: String,
    pub worker: Option<String>,
    pub suggested: Option<String>,
    pub routing_note: Option<String>,
}

/// First segment before `-` groups related subtasks (e.g. `auth-api` + `auth-ui` → `auth`).
pub fn batch_key_for_task(task: &str) -> String {
    if let Some(idx) = task.find('-') {
        let head = &task[..idx];
        if !head.is_empty() {
            return head.to_string();
        }
    }
    task.to_string()
}

fn is_impl_task(task: &str, env_batch: Option<&str>) -> bool {
    if is_meta_task(task) || is_verify_artifact_task(task) {
        return false;
    }
    if let Some(prefix) = env_batch {
        return task.starts_with(prefix);
    }
    true
}

fn routing_note_for(
    agents: &[AgentSpec],
    lead: &str,
    task: &str,
    description: &str,
    worker: Option<&str>,
    project_dir: &Path,
) -> (Option<String>, Option<String>) {
    let suggested = agent_strengths::suggest_worker(
        agents,
        lead,
        task,
        description,
        Some(project_dir),
    )
    .map(|a| a.name.clone());
    let note = worker.and_then(|w| {
        agent_strengths::delegation_mismatch(
            agents,
            lead,
            w,
            task,
            description,
            Some(project_dir),
        )
    });
    (suggested, note)
}

fn description_for_task(snap: &TaskSnapshot) -> String {
    snap.summary.clone().unwrap_or_else(|| snap.task.clone())
}

pub fn collect_batches(project_dir: &Path) -> Vec<BatchSummary> {
    let agents = config::load_agents(project_dir).unwrap_or_default();
    let lead = config::resolve_lead_agent(&agents);
    let env_batch = merge_ready::merge_batch_prefix();
    let states = task_state::load_snapshots(project_dir);

    let mut groups: BTreeMap<String, Vec<TaskRow>> = BTreeMap::new();
    for (task, snap) in states {
        if !is_impl_task(&task, env_batch.as_deref()) {
            continue;
        }
        let key = env_batch
            .as_deref()
            .filter(|p| task.starts_with(p))
            .map(|p| p.to_string())
            .unwrap_or_else(|| batch_key_for_task(&task));
        let desc = description_for_task(&snap);
        let (suggested, routing_note) = routing_note_for(
            &agents,
            &lead,
            &task,
            &desc,
            snap.worker.as_deref(),
            project_dir,
        );
        groups.entry(key).or_default().push(TaskRow {
            task,
            status: snap.status,
            worker: snap.worker,
            suggested,
            routing_note,
        });
    }

    let mut batches: Vec<BatchSummary> = groups
        .into_iter()
        .map(|(id, mut tasks)| {
            tasks.sort_by(|a, b| a.task.cmp(&b.task));
            let done = tasks.iter().filter(|t| t.status == "done").count();
            let blocked = tasks
                .iter()
                .filter(|t| matches!(t.status.as_str(), "blocked" | "failed" | "review_failed"))
                .count();
            let active = tasks.len().saturating_sub(done);
            BatchSummary {
                id,
                tasks,
                done,
                active,
                blocked,
            }
        })
        .collect();

    batches.sort_by(|a, b| {
        b.blocked
            .cmp(&a.blocked)
            .then_with(|| b.active.cmp(&a.active))
            .then_with(|| a.id.cmp(&b.id))
    });
    batches
}

pub fn format_row(row: &TaskRow) -> String {
    let worker = row.worker.as_deref().unwrap_or("-");
    let mut line = format!("  [{}] {} @{}", row.status, row.task, worker);
    if let Some(ref s) = row.suggested {
        if row.worker.as_deref() != Some(s.as_str()) && row.status != "done" {
            line.push_str(&format!(" →建议 @{s}"));
        }
    }
    if let Some(ref note) = row.routing_note {
        let short = note
            .strip_prefix("【委派建议】")
            .unwrap_or(note.as_str());
        let chars: String = short.chars().take(48).collect();
        if short.chars().count() > 48 {
            line.push_str(&format!(" ⚡{chars}…"));
        } else {
            line.push_str(&format!(" ⚡{chars}"));
        }
    }
    line
}

pub fn format_batch_header(batch: &BatchSummary, collapsed: bool) -> String {
    let mark = if collapsed { "▶" } else { "▼" };
    format!(
        "{mark} 批次 {} — {}/{} 完成{}{}",
        batch.id,
        batch.done,
        batch.tasks.len(),
        if batch.blocked > 0 {
            format!(" · {} 受阻", batch.blocked)
        } else {
            String::new()
        },
        if batch.active > 0 && batch.done < batch.tasks.len() {
            format!(" · {} 进行中", batch.active.saturating_sub(batch.blocked))
        } else {
            String::new()
        }
    )
}

pub fn format_grouped_board(
    project_dir: &Path,
    collapsed: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let agents = config::load_agents(project_dir).unwrap_or_default();
    let lead = config::resolve_lead_agent(&agents);
    let env_batch = merge_ready::merge_batch_prefix();
    let mut lines = vec![
        format!("主 Agent: {lead}"),
        format!(
            "批次策略: {}",
            env_batch
                .as_deref()
                .map(|p| format!("环境前缀 `{p}`（AGENT_TUI_MERGE_BATCH）"))
                .unwrap_or_else(|| "按 task 名前缀分组（auth-api + auth-ui → auth）".into())
        ),
        String::from("── 按批次分组（Space 折叠/展开）──"),
    ];

    let batches = collect_batches(project_dir);
    if batches.is_empty() {
        lines.push("  (无实现类任务)".into());
    } else {
        for batch in &batches {
            let collapsed_batch = collapsed.contains(&batch.id);
            lines.push(format_batch_header(batch, collapsed_batch));
            if !collapsed_batch {
                for row in &batch.tasks {
                    lines.push(format_row(row));
                }
            }
        }
    }

    lines.push(String::from("── 任务类型 → Agent 路由 ──"));
    for line in agent_strengths::format_routing_cheatsheet(&agents, &lead)
        .lines()
        .map(str::to_string)
    {
        lines.push(format!("  {line}"));
    }

    lines.push(String::from("── 专家子 Agent ──"));
    for line in crate::specialists::format_roster(project_dir).lines() {
        lines.push(line.to_string());
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_key_splits_on_dash() {
        assert_eq!(batch_key_for_task("auth-api"), "auth");
        assert_eq!(batch_key_for_task("solo"), "solo");
    }
}
