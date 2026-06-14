//! Track whether the lead agent emits a follow-up agent-plan after worker reports.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::lead_watch::{self, LeadWatchState};
use crate::meta::inbox_timestamp_iso;
use crate::pane::AgentPane;
use crate::terminal;

const FOLLOWUP_TAIL_CHARS: usize = 12_000;

fn followup_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/lead_followup.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowupRecord {
    pub time: String,
    pub task: String,
    pub worker: String,
    pub status: String,
    pub satisfied: bool,
    pub nudge_count: u32,
    /// Earliest unix time (due+ prefix) when a PTY nudge may fire.
    #[serde(default)]
    pub due_nudge: String,
}

#[derive(Debug, Clone, Default)]
pub struct FollowupOutcome {
    pub dispatched: usize,
    pub satisfied: usize,
    pub nudged: usize,
}

pub fn followup_enabled() -> bool {
    std::env::var("AGENT_TUI_LEAD_FOLLOWUP")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn followup_timeout_secs() -> u64 {
    std::env::var("AGENT_TUI_LEAD_FOLLOWUP_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120)
        .max(15)
}

fn max_nudges() -> u32 {
    std::env::var("AGENT_TUI_LEAD_FOLLOWUP_MAX_NUDGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
        .min(5)
}

fn transcript_tail(text: &str) -> &str {
    if text.len() <= FOLLOWUP_TAIL_CHARS {
        return text;
    }
    // Safe-ish cut: skip to next char boundary from end.
    let start = text.len() - FOLLOWUP_TAIL_CHARS;
    &text[start..]
}

pub fn load_records(project_dir: &Path, limit: usize) -> Vec<FollowupRecord> {
    let path = followup_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

pub fn pending_records(project_dir: &Path) -> Vec<FollowupRecord> {
    load_records(project_dir, 500)
        .into_iter()
        .filter(|r| !r.satisfied)
        .collect()
}

pub fn pending_count(project_dir: &Path) -> usize {
    pending_records(project_dir).len()
}

pub fn on_worker_report(
    project_dir: &Path,
    worker: &str,
    task: &str,
    status: &str,
) -> Result<()> {
    if !followup_enabled() {
        return Ok(());
    }
    if status != "done" && status != "blocked" && status != "failed" {
        return Ok(());
    }
    let path = followup_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let rec = FollowupRecord {
        time: inbox_timestamp_iso(),
        task: task.to_string(),
        worker: worker.to_string(),
        status: status.to_string(),
        satisfied: false,
        nudge_count: 0,
        due_nudge: due_timestamp(followup_timeout_secs()),
    };
    let line = serde_json::to_string(&rec)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")?;
    terminal::log_message(
        project_dir,
        "info",
        &format!("lead followup: 等待主 Agent 续派（回执 {worker}/{task} {status}）"),
    );
    Ok(())
}

pub fn on_plan_dispatched(project_dir: &Path, dispatched: usize) {
    if !followup_enabled() || dispatched == 0 {
        return;
    }
    let _ = mark_all_satisfied(project_dir, "plan_dispatched");
}

pub fn mark_all_satisfied(project_dir: &Path, reason: &str) -> Result<usize> {
    let path = followup_path(project_dir);
    let Ok(content) = fs::read_to_string(&path) else {
        return Ok(0);
    };
    let mut changed = 0usize;
    let lines: Vec<String> = content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return line.to_string();
            }
            let Ok(mut rec) = serde_json::from_str::<FollowupRecord>(trimmed) else {
                return line.to_string();
            };
            if rec.satisfied {
                return line.to_string();
            }
            rec.satisfied = true;
            changed += 1;
            serde_json::to_string(&rec).unwrap_or_else(|_| line.to_string())
        })
        .collect();
    if changed > 0 {
        fs::write(&path, format!("{}\n", lines.join("\n")))?;
        terminal::log_message(
            project_dir,
            "info",
            &format!("lead followup: {changed} 条已满足（{reason}）"),
        );
    }
    Ok(changed)
}

pub fn transcript_has_agent_plan(text: &str) -> bool {
    transcript_tail(text).contains("```agent-plan") || transcript_tail(text).contains("``` agent-plan")
}

fn due_timestamp(secs_from_now: u64) -> String {
    let due_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() + secs_from_now)
        .unwrap_or(secs_from_now);
    format!("due+{due_secs}")
}

fn is_due(due: &str) -> bool {
    let Some(part) = due.strip_prefix("due+") else {
        return true;
    };
    let Ok(due_secs) = part.parse::<u64>() else {
        return true;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now >= due_secs
}

fn nudge_hint(status: &str) -> &'static str {
    match status {
        "done" => "请 review 产出并续派下一波子任务",
        "failed" => "请安排修复、重试或换人",
        "blocked" => "请决策解除 blocked 或调整计划",
        _ => "请输出下一波 agent-plan",
    }
}

fn build_nudge_line(rec: &FollowupRecord, lead: &str) -> String {
    let hint = nudge_hint(&rec.status);
    format!(
        "[agent-tui·续派] {worker}→{lead} 回执「{task}」({status})。{hint}。输出 ```agent-plan``` JSON 数组，TUI 自动派发。",
        worker = rec.worker,
        lead = lead,
        task = rec.task,
        status = rec.status,
        hint = hint,
    )
}

/// Parse lead transcript tail for new agent-plan blocks and auto-dispatch (followup lane).
pub fn scan_transcript_and_dispatch(
    project_dir: &Path,
    lead: &str,
    agent_names: &[String],
    transcript: &str,
    lead_watch: &mut LeadWatchState,
) -> usize {
    if !followup_enabled() || pending_records(project_dir).is_empty() {
        return 0;
    }
    let tail = transcript_tail(transcript);
    let items = lead_watch::plans_from_text(
        tail,
        agent_names,
        lead_watch.followup_plan_seen_mut(),
    );
    if items.is_empty() {
        return 0;
    }
    lead_watch::dispatch_plan_items(project_dir, lead, items, "followup_transcript")
}

/// Headless entry for verify-loop (no PTY).
pub fn process_transcript_text(
    project_dir: &Path,
    lead: &str,
    agent_names: &[String],
    transcript: &str,
    lead_watch: &mut LeadWatchState,
) -> FollowupOutcome {
    process_pending(
        project_dir,
        lead,
        agent_names,
        Some(transcript),
        None,
        lead_watch,
    )
}

/// While followups are pending: tail-scan transcript → dispatch → satisfy → nudge.
pub fn process_pending(
    project_dir: &Path,
    lead: &str,
    agent_names: &[String],
    transcript: Option<&str>,
    lead_pane: Option<&AgentPane>,
    lead_watch: &mut LeadWatchState,
) -> FollowupOutcome {
    let mut out = FollowupOutcome::default();
    if !followup_enabled() {
        return out;
    }
    if pending_records(project_dir).is_empty() {
        return out;
    }

    let text = transcript
        .map(str::to_string)
        .or_else(|| lead_pane.map(|p| p.transcript_text()));
    if let Some(ref t) = text {
        out.dispatched = scan_transcript_and_dispatch(project_dir, lead, agent_names, t, lead_watch);
        if out.dispatched > 0 {
            out.satisfied = mark_all_satisfied(project_dir, "followup_transcript_dispatch").unwrap_or(0);
            lead_watch.clear_followup_scan();
            return out;
        }
        if transcript_has_agent_plan(t) {
            out.satisfied = mark_all_satisfied(project_dir, "transcript_agent_plan").unwrap_or(0);
            if out.satisfied > 0 {
                lead_watch.clear_followup_scan();
            }
            return out;
        }
    }

    out.nudged = nudge_pending(project_dir, lead, lead_pane);
    out
}

fn nudge_pending(project_dir: &Path, lead: &str, lead_pane: Option<&AgentPane>) -> usize {
    let mut nudged = 0usize;
    let path = followup_path(project_dir);
    let Ok(content) = fs::read_to_string(&path) else {
        return 0;
    };
    let mut lines: Vec<String> = Vec::new();
    let mut dirty = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(mut rec) = serde_json::from_str::<FollowupRecord>(trimmed) else {
            lines.push(line.to_string());
            continue;
        };
        if rec.satisfied {
            lines.push(serde_json::to_string(&rec).unwrap_or_else(|_| line.to_string()));
            continue;
        }
        if is_due(&rec.due_nudge) && rec.nudge_count < max_nudges() {
            if let Some(pane) = lead_pane {
                pane.inject_line(&build_nudge_line(&rec, lead));
                rec.nudge_count += 1;
                rec.due_nudge = due_timestamp(followup_timeout_secs());
                nudged += 1;
                dirty = true;
                terminal::log_message(
                    project_dir,
                    "info",
                    &format!(
                        "lead followup nudge #{}/{} → {lead}（{} 回执「{}」）",
                        rec.nudge_count,
                        max_nudges(),
                        rec.worker,
                        rec.task,
                    ),
                );
            }
        }
        lines.push(serde_json::to_string(&rec).unwrap_or_else(|_| line.to_string()));
    }
    if dirty {
        let _ = fs::write(&path, format!("{}\n", lines.join("\n")));
    }
    nudged
}

/// Back-compat wrapper used by app tick.
pub fn watch_lead_followup(
    project_dir: &Path,
    lead: &str,
    agent_names: &[String],
    lead_pane: Option<&AgentPane>,
    lead_watch: &mut LeadWatchState,
) -> FollowupOutcome {
    let transcript = lead_pane.as_ref().map(|p| p.transcript_text());
    process_pending(
        project_dir,
        lead,
        agent_names,
        transcript.as_deref(),
        lead_pane,
        lead_watch,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_plan_fence_in_tail() {
        assert!(transcript_has_agent_plan("```agent-plan\n[]\n```"));
        assert!(!transcript_has_agent_plan("no plan here"));
    }

    #[test]
    fn nudge_hint_by_status() {
        assert!(nudge_hint("done").contains("review"));
        assert!(nudge_hint("failed").contains("修复"));
    }
}
