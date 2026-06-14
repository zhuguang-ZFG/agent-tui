//! Wrap `gh pr create` after merge-ready (optional automation).

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::config::{load_agents, normalize_windows_path, resolve_lead_agent, worktree_cwd};

#[derive(Debug, Clone)]
pub struct PrCreateOutcome {
    pub created: bool,
    pub url: Option<String>,
    pub message: String,
}

fn gh_available() -> bool {
    Command::new("gh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_branch(cwd: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .context("git branch")?;
    if !out.status.success() {
        bail!("git branch failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn default_title(branch: &str) -> String {
    if branch.is_empty() || branch == "main" || branch == "master" {
        "agent-tui batch".into()
    } else {
        format!("feat: {branch}")
    }
}

/// Create a PR via GitHub CLI from lead worktree (or project root).
pub fn create_pr(
    project_dir: &Path,
    title: Option<&str>,
    body: Option<&str>,
    base: Option<&str>,
    draft: bool,
) -> Result<PrCreateOutcome> {
    if !gh_available() {
        return Ok(PrCreateOutcome {
            created: false,
            url: None,
            message: "未找到 gh CLI。安装 GitHub CLI 后重试，或手动 gh pr create。".into(),
        });
    }

    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let wt = worktree_cwd(project_dir, &lead);
    let cwd = if wt.is_dir() {
        normalize_windows_path(wt)
    } else {
        normalize_windows_path(project_dir.to_path_buf())
    };

    let branch = git_branch(&cwd)?;
    if branch.is_empty() || branch == "main" || branch == "master" {
        return Ok(PrCreateOutcome {
            created: false,
            url: None,
            message: format!(
                "当前分支为「{branch}」，请在 feature 分支上运行 pr-create（或先 checkout -b feature/…）。"
            ),
        });
    }

    let title = title
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_title(&branch));
    let body = body.unwrap_or(
        "Automated PR from agent-tui merge-ready.\n\n\
         - Subtasks completed and reviewed via agent-tui review gate\n\
         - Lead: merge-ready notification",
    );
    let base = base
        .map(str::to_string)
        .unwrap_or_else(|| crate::merge::resolve_base_branch(project_dir));

    let mut cmd = Command::new("gh");
    cmd.current_dir(&cwd);
    cmd.args(["pr", "create", "--title", &title, "--body", body, "--base", &base]);
    if draft {
        cmd.arg("--draft");
    }

    let out = cmd.output().context("gh pr create")?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();

    if out.status.success() {
        let url = if stdout.starts_with("http") {
            Some(stdout.clone())
        } else {
            None
        };
        return Ok(PrCreateOutcome {
            created: true,
            url,
            message: if stdout.is_empty() {
                "PR 已创建。".into()
            } else {
                stdout
            },
        });
    }

    let hint = if stderr.contains("already exists") {
        "（该分支可能已有 PR）"
    } else {
        ""
    };
    bail!("gh pr create 失败{hint}: {stderr}");
}
