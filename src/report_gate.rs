//! Report verification gate before accepting worker `done` status.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::config;
use crate::terminal;

pub struct GateOutcome {
    #[allow(dead_code)]
    pub passed: bool,
    pub status: String,
    pub summary: String,
}

pub fn gate_enabled() -> bool {
    std::env::var("AGENT_TUI_REPORT_GATE")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn verify_command_for(task: &str) -> Option<String> {
    let task_key = task.replace('-', "_").to_uppercase();
    let per_task = format!("AGENT_TUI_VERIFY_{task_key}");
    if let Ok(cmd) = std::env::var(&per_task) {
        if !cmd.trim().is_empty() {
            return Some(cmd);
        }
    }
    std::env::var("AGENT_TUI_VERIFY_CMD")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Run optional verify command in reporter worktree before accepting `done`.
pub fn apply_report_gate(
    project_dir: &Path,
    reporter: &str,
    task: &str,
    status: &str,
    summary: &str,
) -> Result<GateOutcome> {
    if status != "done" || !gate_enabled() {
        return Ok(GateOutcome {
            passed: true,
            status: status.to_string(),
            summary: summary.to_string(),
        });
    }
    let Some(cmd) = verify_command_for(task) else {
        return Ok(GateOutcome {
            passed: true,
            status: status.to_string(),
            summary: summary.to_string(),
        });
    };

    let worktree = config::load_agents(project_dir)
        .ok()
        .and_then(|agents| {
            agents
                .iter()
                .find(|a| a.name.eq_ignore_ascii_case(reporter))
                .map(|a| a.worktree.clone())
        })
        .unwrap_or_else(|| project_dir.to_path_buf());

    terminal::log_message(
        project_dir,
        "info",
        &format!("report gate: {reporter}/{task} → {cmd}"),
    );

    let output = Command::new("cmd")
        .args(["/C", &cmd])
        .current_dir(&worktree)
        .output()
        .with_context(|| format!("run verify cmd in {}", worktree.display()))?;

    if output.status.success() {
        return Ok(GateOutcome {
            passed: true,
            status: status.to_string(),
            summary: summary.to_string(),
        });
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut detail = format!("门禁失败 (exit {:?})", output.status.code());
    if !stdout.trim().is_empty() {
        detail.push_str("\nstdout: ");
        detail.push_str(stdout.trim());
    }
    if !stderr.trim().is_empty() {
        detail.push_str("\nstderr: ");
        detail.push_str(stderr.trim());
    }
    let summary = if summary.is_empty() {
        detail.clone()
    } else {
        format!("{summary}\n{detail}")
    };

    Ok(GateOutcome {
        passed: false,
        status: "failed".into(),
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_non_done() {
        let dir = std::env::temp_dir();
        let out = apply_report_gate(&dir, "codex", "t", "blocked", "x").unwrap();
        assert!(out.passed);
        assert_eq!(out.status, "blocked");
    }
}
