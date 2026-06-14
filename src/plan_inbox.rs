//! Durable plan intake — file channel supplementing PTY transcript scanning.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::lead_watch::{self, PlanItem};
use crate::meta::inbox_timestamp_iso;

fn inbox_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/plan_inbox.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanInboxRecord {
    time: String,
    lead: String,
    worker: String,
    task: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    source: String,
}

pub fn append_item(
    project_dir: &Path,
    lead: &str,
    item: &PlanItem,
    source: &str,
) -> Result<()> {
    let path = inbox_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let rec = PlanInboxRecord {
        time: inbox_timestamp_iso(),
        lead: lead.to_string(),
        worker: item.worker.clone(),
        task: item.task.clone(),
        description: item.description.clone(),
        depends_on: item.depends_on.clone(),
        source: source.to_string(),
    };
    let line = serde_json::to_string(&rec)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

pub fn append_items(
    project_dir: &Path,
    lead: &str,
    items: &[PlanItem],
    source: &str,
) -> Result<()> {
    for item in items {
        append_item(project_dir, lead, item, source)?;
    }
    Ok(())
}

/// Consume new plan_inbox.jsonl lines from `line_offset` onward.
pub fn drain_new_items(
    project_dir: &Path,
    lead: &str,
    agent_names: &[String],
    line_offset: &mut usize,
) -> Vec<PlanItem> {
    let path = inbox_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if *line_offset >= lines.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in &lines[*line_offset..] {
        *line_offset += 1;
        let Ok(rec) = serde_json::from_str::<PlanInboxRecord>(line.trim()) else {
            continue;
        };
        if !rec.lead.eq_ignore_ascii_case(lead) {
            continue;
        }
        if !agent_names
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&rec.worker))
        {
            continue;
        }
        if crate::meta::validate_task_name(&rec.task).is_err() {
            continue;
        }
        out.push(PlanItem {
            worker: rec.worker,
            task: rec.task,
            description: rec.description,
            depends_on: rec.depends_on,
        });
    }
    out
}

/// Append plans via CLI and dispatch immediately.
pub fn submit_items(project_dir: &Path, lead: &str, items: Vec<PlanItem>) -> Result<usize> {
    append_items(project_dir, lead, &items, "cli")?;
    Ok(lead_watch::dispatch_plan_items(
        project_dir,
        lead,
        items,
        "cli",
    ))
}
