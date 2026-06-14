//! Structured team mailbox (append-only JSONL) — OmO / Gas Town–style audit trail.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::meta::inbox_timestamp_iso;

fn mailbox_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/mailbox.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxEntry {
    pub time: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub source: String,
}

pub fn append_entry(
    project_dir: &Path,
    from: &str,
    to: &str,
    kind: &str,
    task: Option<&str>,
    body: &str,
    source: &str,
) -> Result<()> {
    let path = mailbox_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let entry = MailboxEntry {
        time: inbox_timestamp_iso(),
        from: from.to_string(),
        to: to.to_string(),
        kind: kind.to_string(),
        task: task.map(str::to_string),
        body: body.to_string(),
        source: source.to_string(),
    };
    let line = serde_json::to_string(&entry)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

pub fn load_entries(project_dir: &Path, limit: usize) -> Vec<MailboxEntry> {
    let all = load_all_entries(project_dir);
    let start = all.len().saturating_sub(limit);
    all[start..].to_vec()
}

pub fn load_all_entries(project_dir: &Path) -> Vec<MailboxEntry> {
    let path = mailbox_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect()
}

/// Entries starting at `line_offset` (0-based line index).
pub fn load_entries_from_line(project_dir: &Path, line_offset: usize) -> Vec<MailboxEntry> {
    load_all_entries(project_dir)
        .into_iter()
        .skip(line_offset)
        .collect()
}

pub fn format_entry(e: &MailboxEntry) -> String {
    let time = if e.time.len() >= 19 {
        &e.time[11..19]
    } else {
        e.time.as_str()
    };
    let task = e
        .task
        .as_deref()
        .map(|t| format!(" 「{t}」"))
        .unwrap_or_default();
    let body = truncate(&e.body.replace('\n', " "), 56);
    format!(
        "  {time} [{kind}] {from} → {to}{task} {body}",
        kind = e.kind,
        from = e.from,
        to = e.to
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!(
        "{}…",
        s.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

pub fn user_task(project_dir: &Path, lead: &str, message: &str) -> Result<()> {
    append_entry(
        project_dir,
        "user",
        lead,
        "user_task",
        None,
        message,
        "inbox",
    )
}

pub fn plan_dispatch(
    project_dir: &Path,
    lead: &str,
    worker: &str,
    task: &str,
    description: &str,
    source: &str,
) -> Result<()> {
    append_entry(
        project_dir,
        lead,
        worker,
        "plan",
        Some(task),
        description,
        source,
    )
}

pub fn delegate(
    project_dir: &Path,
    lead: &str,
    worker: &str,
    task: &str,
    description: &str,
) -> Result<()> {
    append_entry(
        project_dir,
        lead,
        worker,
        "delegate",
        Some(task),
        description,
        "delegation",
    )
}

pub fn report(
    project_dir: &Path,
    reporter: &str,
    lead: &str,
    task: &str,
    status: &str,
    summary: &str,
) -> Result<()> {
    append_entry(
        project_dir,
        reporter,
        lead,
        "report",
        Some(task),
        &format!("[{status}] {summary}"),
        "auto_report",
    )
}
