use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::meta::{append_coord_event, append_shared_line, inbox_timestamp_iso};

pub use crate::meta::validate_task_name;

#[derive(Debug, Clone, Deserialize)]
struct ClaimRecord {
    #[serde(rename = "type")]
    kind: String,
    task: String,
    agent: String,
    action: String,
}

#[derive(Debug, Clone, Default)]
pub struct ClaimsSnapshot {
    /// agent -> active task names (claim without release)
    pub agent_tasks: HashMap<String, Vec<String>>,
    /// tasks with more than one active claimant
    pub conflict_tasks: HashSet<String>,
}

pub fn claims_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/claims.jsonl")
}

pub fn load_claims_snapshot(project_dir: &Path) -> ClaimsSnapshot {
    let path = claims_path(project_dir);
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return ClaimsSnapshot::default(),
    };

    // task -> set of agents with active claim
    let mut active: HashMap<String, HashSet<String>> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<ClaimRecord>(line) else {
            continue;
        };
        if rec.kind != "claim" {
            continue;
        }
        let task = rec.task.trim().to_string();
        let agent = rec.agent.trim().to_string();
        if task.is_empty() || agent.is_empty() {
            continue;
        }
        let agents = active.entry(task).or_default();
        match rec.action.as_str() {
            "claim" => {
                agents.insert(agent);
            }
            "release" => {
                agents.remove(&agent);
            }
            _ => {}
        }
    }

    let mut agent_tasks: HashMap<String, Vec<String>> = HashMap::new();
    let mut conflict_tasks = HashSet::new();

    for (task, agents) in active {
        if agents.is_empty() {
            continue;
        }
        if agents.len() > 1 {
            conflict_tasks.insert(task.clone());
        }
        for agent in agents {
            agent_tasks.entry(agent).or_default().push(task.clone());
        }
    }

    for tasks in agent_tasks.values_mut() {
        tasks.sort();
        tasks.dedup();
    }

    ClaimsSnapshot {
        agent_tasks,
        conflict_tasks,
    }
}

pub fn agent_has_conflict(snapshot: &ClaimsSnapshot, agent: &str) -> bool {
    snapshot
        .agent_tasks
        .get(agent)
        .into_iter()
        .flatten()
        .any(|t| snapshot.conflict_tasks.contains(t))
}

pub fn claim_task(project_dir: &Path, agent: &str, task: &str) -> Result<()> {
    let task = task.trim();
    validate_task_name(task)?;
    let snapshot = load_claims_snapshot(project_dir);
    if snapshot
        .agent_tasks
        .get(agent)
        .is_some_and(|tasks| tasks.iter().any(|t| t == task))
    {
        return Ok(());
    }
    append_claim_event(project_dir, agent, task, "claim")?;

    let snapshot = load_claims_snapshot(project_dir);
    let holders: Vec<String> = snapshot
        .agent_tasks
        .iter()
        .filter_map(|(a, tasks)| {
            if tasks.iter().any(|t| t == task) {
                Some(a.clone())
            } else {
                None
            }
        })
        .collect();

    if holders.len() > 1 {
        append_shared_line(
            project_dir,
            &format!("⚠ 任务冲突「{task}」：{} 同时认领", holders.join("、")),
        )?;
    } else {
        append_shared_line(project_dir, &format!("{agent} 认领任务「{task}」"))?;
    }
    Ok(())
}

pub fn release_task(project_dir: &Path, agent: &str, task: &str) -> Result<()> {
    let task = task.trim();
    validate_task_name(task)?;
    append_claim_event(project_dir, agent, task, "release")?;
    append_shared_line(project_dir, &format!("{agent} 释放任务「{task}」"))?;
    Ok(())
}

fn append_claim_event(project_dir: &Path, agent: &str, task: &str, action: &str) -> Result<()> {
    std::fs::create_dir_all(project_dir.join(".agents/shared"))
        .context("创建 .agents/shared")?;
    let iso = inbox_timestamp_iso();
    let event = serde_json::json!({
        "time": iso,
        "type": "claim",
        "task": task,
        "agent": agent,
        "action": action,
    });
    let path = claims_path(project_dir);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("打开 {}", path.display()))?;
    writeln!(file, "{event}")?;

    let msg = if action == "release" {
        format!("{agent} 释放任务「{task}」")
    } else {
        format!("{agent} 认领任务「{task}」")
    };
    append_coord_event(
        project_dir,
        "claim",
        Some(agent),
        &msg,
        "system",
        Some(task),
        Some(action),
    )?;
    Ok(())
}
