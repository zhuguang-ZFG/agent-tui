//! Optional auto `gh pr create` after merge-ready (+ smoke gate).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::{load_agents, resolve_lead_agent};
use crate::merge_ready;
use crate::meta;
use crate::post_merge_smoke;
use crate::pr_create::{self, PrCreateOutcome};
use crate::terminal;

fn dispatched_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/auto_pr_dispatched.jsonl")
}

pub fn auto_pr_enabled() -> bool {
    std::env::var("AGENT_TUI_AUTO_PR")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
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

pub fn was_auto_pr_dispatched(project_dir: &Path, done_tasks: &[String]) -> bool {
    let fp = merge_ready::done_tasks_fingerprint(done_tasks);
    already_dispatched(project_dir, &fp)
}

/// Create PR when enabled, merge-ready notified, smoke passed (if required), and on feature branch.
pub fn maybe_create(project_dir: &Path, done_tasks: &[String]) -> Result<Option<PrCreateOutcome>> {
    if !auto_pr_enabled() {
        return Ok(None);
    }
    if !merge_ready::was_notified_for_done_tasks(project_dir, done_tasks) {
        return Ok(None);
    }
    if !post_merge_smoke::smoke_gate_satisfied(project_dir, done_tasks) {
        return Ok(None);
    }

    let fp = merge_ready::done_tasks_fingerprint(done_tasks);
    if already_dispatched(project_dir, &fp) {
        return Ok(None);
    }

    let base = crate::merge::resolve_base_branch(project_dir);
    let outcome = pr_create::create_pr(project_dir, None, None, Some(&base), false)?;
    remember_dispatched(project_dir, &fp)?;

    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    if outcome.created {
        let url = outcome
            .url
            .as_deref()
            .unwrap_or("(见 gh 输出)");
        let body = format!(
            "【auto-pr】已自动创建 PR：{url}\n\
             ▶ Lead：在 GitHub review 后 merge；合并后可派 post-merge 验证 task。"
        );
        let _ = meta::notify_agent_from(project_dir, &lead, &body, "agent-tui");
        terminal::log_message(project_dir, "info", &format!("auto-pr: {url}"));
    } else {
        terminal::log_message(
            project_dir,
            "warn",
            &format!("auto-pr 跳过: {}", outcome.message),
        );
    }

    Ok(Some(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("auto-pr-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        dir
    }

    #[test]
    fn disabled_by_default() {
        std::env::remove_var("AGENT_TUI_AUTO_PR");
        let dir = fresh_dir("off");
        let done = vec!["a".into()];
        assert!(maybe_create(&dir, &done).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
