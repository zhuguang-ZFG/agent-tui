//! Dead-letter queue and automatic retry for failed worker reports.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::claims;
use crate::config;
use crate::delegation;
use crate::meta::inbox_timestamp_iso;
use crate::terminal;

fn dead_letter_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/dead_letter.jsonl")
}

fn retry_queue_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/retry_queue.jsonl")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryBackoff {
    Fixed,
    Exponential,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetterRecord {
    pub time: String,
    pub task: String,
    pub worker: String,
    pub lead: String,
    pub status: String,
    pub summary: String,
    pub attempt: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RetryRecord {
    time: String,
    due: String,
    task: String,
    worker: String,
    lead: String,
    description: String,
    attempt: u32,
    #[serde(default)]
    previous_worker: Option<String>,
    #[serde(default)]
    cooldown_secs: Option<u64>,
}

pub fn auto_retry_enabled() -> bool {
    std::env::var("AGENT_TUI_AUTO_RETRY")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

pub fn max_retries() -> u32 {
    std::env::var("AGENT_TUI_MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
        .clamp(0, 5)
}

pub fn retry_rotate_enabled() -> bool {
    std::env::var("AGENT_TUI_RETRY_ROTATE_WORKER")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

pub fn retry_backoff() -> RetryBackoff {
    match std::env::var("AGENT_TUI_RETRY_BACKOFF")
        .unwrap_or_else(|_| "exponential".into())
        .to_lowercase()
        .as_str()
    {
        "fixed" => RetryBackoff::Fixed,
        _ => RetryBackoff::Exponential,
    }
}

fn retry_base_cooldown_secs() -> u64 {
    std::env::var("AGENT_TUI_RETRY_COOLDOWN_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(45)
        .max(1)
}

/// Cooldown before attempt N retry (attempt is 1-based dead-letter count).
pub fn compute_cooldown_secs(attempt: u32) -> u64 {
    let base = retry_base_cooldown_secs();
    match retry_backoff() {
        RetryBackoff::Fixed => base,
        RetryBackoff::Exponential => {
            let exp = attempt.saturating_sub(1).min(6);
            base.saturating_mul(1u64 << exp)
        }
    }
}

/// Worker pool for rotation: all enabled agents except lead.
pub fn worker_pool(project_dir: &Path, lead: &str) -> Vec<String> {
    config::load_agents(project_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !a.name.eq_ignore_ascii_case(lead))
        .map(|a| a.name)
        .collect()
}

/// Pick worker for retry attempt; rotates round-robin when enabled.
pub fn pick_retry_worker(
    project_dir: &Path,
    lead: &str,
    failed_worker: &str,
    attempt: u32,
) -> String {
    if !retry_rotate_enabled() {
        return failed_worker.to_string();
    }
    let pool = worker_pool(project_dir, lead);
    if pool.len() <= 1 {
        return failed_worker.to_string();
    }
    let base = pool
        .iter()
        .position(|w| w.eq_ignore_ascii_case(failed_worker))
        .unwrap_or(0);
    let offset = attempt.max(1) as usize;
    pool[(base + offset) % pool.len()].clone()
}

pub fn load_dead_letters(project_dir: &Path, limit: usize) -> Vec<DeadLetterRecord> {
    let path = dead_letter_path(project_dir);
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

fn count_attempts(project_dir: &Path, task: &str) -> u32 {
    load_dead_letters(project_dir, 1000)
        .into_iter()
        .filter(|r| r.task == task)
        .map(|r| r.attempt)
        .max()
        .unwrap_or(0)
}

pub fn record_and_maybe_retry(
    project_dir: &Path,
    lead: &str,
    worker: &str,
    task: &str,
    status: &str,
    summary: &str,
    description: &str,
) -> Result<()> {
    if status != "failed" && status != "blocked" {
        return Ok(());
    }

    let attempt = count_attempts(project_dir, task) + 1;
    let rec = DeadLetterRecord {
        time: inbox_timestamp_iso(),
        task: task.to_string(),
        worker: worker.to_string(),
        lead: lead.to_string(),
        status: status.to_string(),
        summary: summary.to_string(),
        attempt,
    };
    let path = dead_letter_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(&rec)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")?;

    terminal::log_message(
        project_dir,
        "warn",
        &format!("dead-letter {worker}/{task} status={status} attempt={attempt}"),
    );

    if status == "failed" && auto_retry_enabled() && attempt <= max_retries() {
        let target = pick_retry_worker(project_dir, lead, worker, attempt);
        let cooldown = compute_cooldown_secs(attempt);
        schedule_retry(
            project_dir,
            lead,
            &target,
            worker,
            task,
            description,
            attempt,
            cooldown,
        )?;
        terminal::log_message(
            project_dir,
            "info",
            &format!(
                "已排程重试 {target}/{task}（原 {worker}，{attempt}/{}，{}s 后，{}退避）",
                max_retries(),
                cooldown,
                match retry_backoff() {
                    RetryBackoff::Fixed => "固定",
                    RetryBackoff::Exponential => "指数",
                }
            ),
        );
    }
    Ok(())
}

fn schedule_retry(
    project_dir: &Path,
    lead: &str,
    worker: &str,
    previous_worker: &str,
    task: &str,
    description: &str,
    attempt: u32,
    cooldown_secs: u64,
) -> Result<()> {
    let path = retry_queue_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let due = due_timestamp(cooldown_secs);
    let prev = if worker.eq_ignore_ascii_case(previous_worker) {
        None
    } else {
        Some(previous_worker.to_string())
    };
    let rec = RetryRecord {
        time: inbox_timestamp_iso(),
        due,
        task: task.to_string(),
        worker: worker.to_string(),
        lead: lead.to_string(),
        description: description.to_string(),
        attempt,
        previous_worker: prev,
        cooldown_secs: Some(cooldown_secs),
    };
    let line = serde_json::to_string(&rec)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")?;
    Ok(())
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

pub fn process_retry_queue(project_dir: &Path, lead: &str) -> usize {
    process_retry_queue_inner(project_dir, lead, false)
}

/// Headless verify-loop: execute all due retries ignoring cooldown timestamps.
pub fn process_retry_queue_immediate(project_dir: &Path, lead: &str) -> usize {
    process_retry_queue_inner(project_dir, lead, true)
}

pub fn pending_retry_count(project_dir: &Path, task: &str) -> usize {
    load_retry_queue(project_dir, 1000)
        .into_iter()
        .filter(|r| r.task == task)
        .count()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryQueueEntry {
    pub task: String,
    pub worker: String,
    pub previous_worker: Option<String>,
    pub attempt: u32,
    pub cooldown_secs: Option<u64>,
}

pub fn load_retry_queue(project_dir: &Path, limit: usize) -> Vec<RetryQueueEntry> {
    let path = retry_queue_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<RetryRecord>(l.trim()).ok())
        .rev()
        .take(limit)
        .map(|r| RetryQueueEntry {
            task: r.task,
            worker: r.worker,
            previous_worker: r.previous_worker,
            attempt: r.attempt,
            cooldown_secs: r.cooldown_secs,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn process_retry_queue_inner(project_dir: &Path, lead: &str, ignore_cooldown: bool) -> usize {
    if !auto_retry_enabled() {
        return 0;
    }
    let path = retry_queue_path(project_dir);
    let Ok(content) = fs::read_to_string(&path) else {
        return 0;
    };
    let lines: Vec<String> = content.lines().map(str::to_string).collect();
    if lines.is_empty() {
        return 0;
    }

    let mut kept = Vec::new();
    let mut executed = 0usize;
    for line in lines {
        let line_trim = line.trim();
        if line_trim.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<RetryRecord>(line_trim) else {
            kept.push(line);
            continue;
        };
        if !rec.lead.eq_ignore_ascii_case(lead) {
            kept.push(line);
            continue;
        }
        if !ignore_cooldown && !is_due(&rec.due) {
            kept.push(line);
            continue;
        }
        if let Some(prev) = rec.previous_worker.as_deref() {
            if !prev.eq_ignore_ascii_case(&rec.worker) {
                let _ = claims::release_task(project_dir, prev, &rec.task);
            }
        }
        let desc = if rec.description.trim().is_empty() {
            format!(
                "[重试 #{}/{}] 请修复失败任务并重新完成",
                rec.attempt,
                max_retries()
            )
        } else {
            format!(
                "[重试 #{}/{}] {}",
                rec.attempt,
                max_retries(),
                rec.description.trim()
            )
        };
        match delegation::delegate_task(
            project_dir,
            lead,
            &rec.worker,
            &rec.task,
            &desc,
        ) {
            Ok(()) => {
                executed += 1;
                let rotate_note = rec
                    .previous_worker
                    .as_ref()
                    .filter(|p| !p.eq_ignore_ascii_case(&rec.worker))
                    .map(|p| format!("（原 {p}）"))
                    .unwrap_or_default();
                terminal::log_message(
                    project_dir,
                    "info",
                    &format!(
                        "auto-retry 已重新委派 {} → {}{rotate_note}",
                        rec.worker, rec.task
                    ),
                );
            }
            Err(e) => {
                terminal::log_message(
                    project_dir,
                    "warn",
                    &format!("auto-retry {}/{} failed: {e:#}", rec.worker, rec.task),
                );
                kept.push(line);
            }
        }
    }

    if kept.is_empty() {
        let _ = fs::remove_file(&path);
    } else {
        let _ = fs::write(&path, format!("{}\n", kept.join("\n")));
    }
    executed
}

pub fn format_dead_letter(r: &DeadLetterRecord) -> String {
    let time = if r.time.len() >= 19 {
        &r.time[11..19]
    } else {
        r.time.as_str()
    };
    let summary: String = r.summary.chars().take(40).collect();
    format!(
        "  {time} {worker} → {lead} 「{task}」 [{status}] attempt={attempt} {summary}",
        worker = r.worker,
        lead = r.lead,
        task = r.task,
        status = r.status,
        attempt = r.attempt,
        summary = summary,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_backoff_doubles() {
        std::env::set_var("AGENT_TUI_RETRY_BACKOFF", "exponential");
        std::env::set_var("AGENT_TUI_RETRY_COOLDOWN_SECS", "10");
        assert_eq!(compute_cooldown_secs(1), 10);
        assert_eq!(compute_cooldown_secs(2), 20);
        assert_eq!(compute_cooldown_secs(3), 40);
    }

    #[test]
    fn fixed_backoff_uses_base() {
        std::env::set_var("AGENT_TUI_RETRY_BACKOFF", "fixed");
        std::env::set_var("AGENT_TUI_RETRY_COOLDOWN_SECS", "30");
        assert_eq!(compute_cooldown_secs(1), 30);
        assert_eq!(compute_cooldown_secs(3), 30);
    }
}
