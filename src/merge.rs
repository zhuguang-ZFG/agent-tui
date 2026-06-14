//! Wrap agents-complete `merge.sh` (+ non-interactive `--all`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::config::{load_agents, normalize_windows_path};

#[derive(Debug, Clone)]
pub struct MergeOutcome {
    #[allow(dead_code)]
    pub mode: String,
    pub message: String,
    pub merge_branch: Option<String>,
    pub merged_agents: Vec<String>,
}

pub fn resolve_bash() -> Option<PathBuf> {
    for key in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(root) = std::env::var(key) {
            let bash = PathBuf::from(&root).join("Git").join("bin").join("bash.exe");
            if bash.is_file() {
                return Some(bash);
            }
        }
    }
    which_bash_unix()
}

fn which_bash_unix() -> Option<PathBuf> {
    Command::new("bash")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| PathBuf::from("bash"))
}

fn merge_script(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/merge.sh")
}

fn read_base_branch(project_dir: &Path) -> String {
    let path = project_dir.join(".agents/.base-branch");
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "main".into())
}

/// Resolve default base branch for merge / PR (`.agents/.base-branch` or `main`).
pub fn resolve_base_branch(project_dir: &Path) -> String {
    read_base_branch(project_dir)
}

/// Reject git option injection and invalid ref names from agent-writable files.
pub fn validate_git_ref_name(name: &str) -> Result<()> {
    let s = name.trim();
    if s.is_empty() {
        bail!("git ref 不能为空");
    }
    if s.starts_with('-') {
        bail!("git ref 不能以 '-' 开头: {s}");
    }
    if s.chars().any(char::is_whitespace) {
        bail!("git ref 不能含空白: {s}");
    }
    for bad in ["..", "@{", "~", "^", ":", "\\", "[", "?", "*"] {
        if s.contains(bad) {
            bail!("git ref 含非法字符 ({bad}): {s}");
        }
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        bail!("git ref 含不允许的字符: {s}");
    }
    Ok(())
}

fn git_run(project_dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(project_dir)
        .output()
        .with_context(|| format!("git {}", args.join(" ")))
}

fn git_ok(project_dir: &Path, args: &[&str]) -> Result<()> {
    let out = git_run(project_dir, args)?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {} failed: {stderr}", args.join(" "))
    }
}

fn agent_branch(project_dir: &Path, agent: &str) -> Option<String> {
    let path = project_dir.join(".agents").join(agent).join("branch.txt");
    let branch = fs::read_to_string(path).ok()?.trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

/// If the agent worktree has uncommitted changes, auto-commit them so that
/// `merge --all` sees the latest state. Mirrors agents-complete/merge.sh.
fn snapshot_agent_worktree(project_dir: &Path, agent: &str) -> Result<bool> {
    let worktree = project_dir.join(".agents").join(agent).join("worktree");
    if !worktree.join(".git").exists() {
        return Ok(false);
    }
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&worktree)
        .output()
        .with_context(|| format!("git status --porcelain in {}", worktree.display()))?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        bail!("无法检查 {agent} worktree 状态: {stderr}");
    }
    if status.stdout.is_empty() {
        return Ok(false);
    }
    git_ok(&worktree, &["add", "-A"])
        .with_context(|| format!("snapshot add for {agent}"))?;
    let msg = format!("auto-snapshot before merge for agent {agent}");
    git_ok(&worktree, &["commit", "-m", &msg])
        .with_context(|| format!("snapshot commit for {agent}"))?;
    Ok(true)
}

fn merge_branch_name() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("merge-{secs}")
}

/// Non-interactive: merge all agent branches into one merge-* branch at repo root.
pub fn merge_all_agents(project_dir: &Path, base: Option<&str>) -> Result<MergeOutcome> {
    let project_dir = normalize_windows_path(
        project_dir
            .canonicalize()
            .unwrap_or_else(|_| project_dir.to_path_buf()),
    );
    let base = base
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| read_base_branch(&project_dir));
    validate_git_ref_name(&base)?;

    git_ok(&project_dir, &["fetch", "origin", &base])?;

    let merge_branch = merge_branch_name();
    git_ok(
        &project_dir,
        &["checkout", "-B", &merge_branch, &base],
    )?;

    let agents = load_agents(&project_dir)?;
    let mut merged = Vec::new();
    for agent in &agents {
        let Some(branch) = agent_branch(&project_dir, &agent.name) else {
            continue;
        };
        if branch == base {
            continue;
        }
        validate_git_ref_name(&branch)
            .with_context(|| format!("agent {} branch.txt", agent.name))?;
        snapshot_agent_worktree(&project_dir, &agent.name)
            .with_context(|| format!("agent {} worktree snapshot", agent.name))?;
        let msg = format!("Merge {}: {}", agent.name, branch);
        let out = git_run(
            &project_dir,
            &["merge", &branch, "--no-edit", "-m", &msg],
        )?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            git_run(&project_dir, &["merge", "--abort"]).ok();
            bail!(
                "合并 {} ({}) 冲突或失败: {stderr}。解决后手动 merge 或运行交互式 agent-tui merge",
                agent.name,
                branch
            );
        }
        merged.push(format!("{}:{}", agent.name, branch));
    }

    Ok(MergeOutcome {
        mode: "all".into(),
        message: format!(
            "已创建合并分支 {merge_branch}（基于 {base}），合并 {} 个 agent 分支。\n\
             下一步: git push -u origin {merge_branch} 或 agent-tui pr-create",
            merged.len()
        ),
        merge_branch: Some(merge_branch),
        merged_agents: merged,
    })
}

/// Interactive wrap of `.agents/merge.sh` (agents-complete).
pub fn merge_interactive(project_dir: &Path) -> Result<MergeOutcome> {
    let project_dir = normalize_windows_path(
        project_dir
            .canonicalize()
            .unwrap_or_else(|_| project_dir.to_path_buf()),
    );
    let script = merge_script(&project_dir);
    if !script.is_file() {
        bail!(
            "未找到 {}。先运行 agents-complete / clideckctl init，或使用 agent-tui merge --all",
            script.display()
        );
    }
    let bash = resolve_bash().context(
        "需要 Git Bash（Windows）或 bash。安装 Git for Windows，或使用 agent-tui merge --all",
    )?;
    let status = Command::new(&bash)
        .arg(script)
        .current_dir(&project_dir)
        .status()
        .context("merge.sh")?;
    if !status.success() {
        bail!("merge.sh 退出码 {}", status.code().unwrap_or(-1));
    }
    Ok(MergeOutcome {
        mode: "interactive".into(),
        message: "merge.sh 已完成（交互式合并）。".into(),
        merge_branch: None,
        merged_agents: vec![],
    })
}

pub fn run_merge(project_dir: &Path, all: bool, base: Option<&str>) -> Result<MergeOutcome> {
    if all {
        merge_all_agents(project_dir, base)
    } else {
        merge_interactive(project_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_branch_name_unique_enough() {
        let a = merge_branch_name();
        let b = merge_branch_name();
        assert!(a.starts_with("merge-"));
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_git_option_injection_in_ref() {
        assert!(validate_git_ref_name("--upload-pack=evil").is_err());
        assert!(validate_git_ref_name("-main").is_err());
        assert!(validate_git_ref_name("feature/foo").is_ok());
    }
}
