//! PR lifecycle: poll merge status, post-GitHub-merge verification.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::auto_pr;
use crate::config::{load_agents, normalize_windows_path, resolve_lead_agent, worktree_cwd};
use crate::delegation;
use crate::merge_ready;
use crate::meta;
use crate::post_merge_smoke;
use crate::review_gate::pick_reviewer;
use crate::task_state;
use crate::terminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    None,
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone)]
pub struct PrStatusReport {
    pub state: PrState,
    pub branch: String,
    pub url: Option<String>,
    pub message: String,
    pub checks_passing: bool,
    pub checks_pending: bool,
    pub review_approved: bool,
    pub mergeable: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct GhCheckItem {
    #[serde(default)]
    state: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct GhPrView {
    #[serde(default)]
    state: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "reviewDecision", default)]
    review_decision: String,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: String,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<GhCheckItem>,
}

fn ci_pass_simulate_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/pr_ci_pass_simulate.json")
}

fn auto_merge_dispatched_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/auto_merge_dispatched.jsonl")
}

/// Test hook: CI + review 满足，触发 auto-merge 门禁（无 gh 时走 simulate merge）。
pub fn simulate_ci_pass(project_dir: &Path) -> Result<()> {
    let path = ci_pass_simulate_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, r#"{"ci":true,"review":true}"#)?;
    Ok(())
}

fn ci_pass_simulated(project_dir: &Path) -> bool {
    ci_pass_simulate_path(project_dir).is_file()
}

pub fn auto_merge_enabled() -> bool {
    std::env::var("AGENT_TUI_AUTO_MERGE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn auto_merge_squash() -> bool {
    std::env::var("AGENT_TUI_AUTO_MERGE_SQUASH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn skip_review_required() -> bool {
    std::env::var("AGENT_TUI_AUTO_MERGE_SKIP_REVIEW")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn already_auto_merged(project_dir: &Path, fp: &str) -> bool {
    let path = auto_merge_dispatched_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    content.lines().any(|l| l.trim() == fp)
}

fn remember_auto_merged(project_dir: &Path, fp: &str) -> Result<()> {
    let path = auto_merge_dispatched_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{fp}")?;
    Ok(())
}

pub fn was_auto_merge_dispatched(project_dir: &Path, done_tasks: &[String]) -> bool {
    let fp = merge_ready::done_tasks_fingerprint(done_tasks);
    already_auto_merged(project_dir, &fp)
}

fn parse_gh_pr_view(raw: &str, branch: &str) -> PrStatusReport {
    let empty = PrStatusReport {
        state: PrState::None,
        branch: branch.to_string(),
        url: None,
        message: "无活动 PR".into(),
        checks_passing: false,
        checks_pending: false,
        review_approved: false,
        mergeable: false,
    };
    let Ok(view) = serde_json::from_str::<GhPrView>(raw) else {
        return empty;
    };

    let mut checks_pending = false;
    let mut checks_passing = true;
    if view.status_check_rollup.is_empty() {
        checks_passing = true;
    } else {
        for c in &view.status_check_rollup {
            match c.state.as_str() {
                "SUCCESS" | "NEUTRAL" | "SKIPPING" => {}
                "PENDING" | "IN_PROGRESS" | "QUEUED" | "WAITING" | "REQUESTED" => {
                    checks_pending = true;
                    checks_passing = false;
                }
                _ => checks_passing = false,
            }
        }
    }

    let review_approved = view.review_decision == "APPROVED"
        || (skip_review_required() && view.review_decision != "CHANGES_REQUESTED");
    let mergeable = matches!(view.merge_state_status.as_str(), "CLEAN" | "HAS_HOOKS")
        || view.merge_state_status.is_empty();

    let state = match view.state.as_str() {
        "OPEN" => PrState::Open,
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::None,
    };

    let url = (!view.url.is_empty()).then_some(view.url);
    let message = match state {
        PrState::Open => {
            if checks_pending {
                format!("PR 待合并，CI 运行中（{branch}）")
            } else if !checks_passing {
                format!("PR 待合并，CI 未通过（{branch}）")
            } else if !review_approved {
                format!("PR 待合并，等待 review 批准（{branch}）")
            } else {
                format!("PR 可合并（CI ✓ review ✓，{branch}）")
            }
        }
        PrState::Merged => format!("PR 已合并（{branch}）"),
        PrState::Closed => format!("PR 已关闭未合并（{branch}）"),
        PrState::None => "无活动 PR".into(),
    };

    PrStatusReport {
        state,
        branch: branch.to_string(),
        url,
        message,
        checks_passing,
        checks_pending,
        review_approved,
        mergeable,
    }
}

fn simulated_open_report(branch: &str) -> PrStatusReport {
    PrStatusReport {
        state: PrState::Open,
        branch: branch.to_string(),
        url: None,
        message: "（simulate）PR 待合并，CI/review 已通过".into(),
        checks_passing: true,
        checks_pending: false,
        review_approved: true,
        mergeable: true,
    }
}

fn gh_available() -> bool {
    Command::new("gh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn lead_git_cwd(project_dir: &Path) -> PathBuf {
    let agents = load_agents(project_dir).ok();
    let lead = agents
        .as_ref()
        .map(|a| resolve_lead_agent(a))
        .unwrap_or_else(|| "cursor".into());
    let wt = worktree_cwd(project_dir, &lead);
    if wt.is_dir() {
        normalize_windows_path(wt)
    } else {
        normalize_windows_path(project_dir.to_path_buf())
    }
}

fn git_branch(cwd: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .context("git branch")?;
    if !out.status.success() {
        anyhow::bail!("git branch failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn simulate_merged_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/pr_merged_simulate.json")
}

/// Test hook: touch file to simulate merged PR without gh.
pub fn simulate_merged(project_dir: &Path) -> Result<()> {
    let path = simulate_merged_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, r#"{"merged":true}"#)?;
    Ok(())
}

fn pr_merged_simulated(project_dir: &Path) -> bool {
    simulate_merged_path(project_dir).is_file()
}

pub fn poll_pr_enabled() -> bool {
    std::env::var("AGENT_TUI_POLL_PR")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

pub fn post_github_merge_task_id(done_tasks: &[String]) -> String {
    let prefix = merge_ready::merge_batch_prefix().unwrap_or_else(|| "batch".into());
    let fp = merge_ready::done_tasks_fingerprint(done_tasks);
    let hash = fp
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    format!("{prefix}-post-github-merge-{hash:08x}")
}

fn dispatched_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/post_github_merge_dispatched.jsonl")
}

fn notified_merged_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/pr_merged_notified.jsonl")
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

fn already_notified_merged(project_dir: &Path, fp: &str) -> bool {
    let path = notified_merged_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    content.lines().any(|l| l.trim() == fp)
}

fn remember_notified_merged(project_dir: &Path, fp: &str) -> Result<()> {
    let path = notified_merged_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{fp}")?;
    Ok(())
}

pub fn post_github_merge_passed(project_dir: &Path, done_tasks: &[String]) -> bool {
    let id = post_github_merge_task_id(done_tasks);
    task_state::load_snapshots(project_dir)
        .get(&id)
        .map(|s| s.status == "done")
        .unwrap_or(false)
}

pub fn query_pr_status(project_dir: &Path) -> Result<PrStatusReport> {
    if pr_merged_simulated(project_dir) {
        return Ok(PrStatusReport {
            state: PrState::Merged,
            branch: String::new(),
            url: None,
            message: "（simulate）PR 已合并".into(),
            checks_passing: true,
            checks_pending: false,
            review_approved: true,
            mergeable: true,
        });
    }

    if ci_pass_simulated(project_dir) {
        let branch = git_branch(&lead_git_cwd(project_dir)).unwrap_or_default();
        return Ok(simulated_open_report(&branch));
    }

    let cwd = lead_git_cwd(project_dir);
    let branch = git_branch(&cwd).unwrap_or_default();
    if branch.is_empty() || branch == "main" || branch == "master" {
        return Ok(PrStatusReport {
            state: PrState::None,
            branch,
            url: None,
            message: "当前在 main/master，无 feature PR 可查询。".into(),
            checks_passing: false,
            checks_pending: false,
            review_approved: false,
            mergeable: false,
        });
    }

    if !gh_available() {
        return Ok(PrStatusReport {
            state: PrState::None,
            branch,
            url: None,
            message: "未安装 gh CLI，无法查询 PR 状态。".into(),
            checks_passing: false,
            checks_pending: false,
            review_approved: false,
            mergeable: false,
        });
    }

    let out = Command::new("gh")
        .args([
            "pr",
            "view",
            "--json",
            "state,url,reviewDecision,mergeStateStatus,statusCheckRollup",
        ])
        .current_dir(&cwd)
        .output()
        .context("gh pr view")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Ok(PrStatusReport {
            state: PrState::None,
            branch,
            url: None,
            message: format!("无 PR 或查询失败: {stderr}"),
            checks_passing: false,
            checks_pending: false,
            review_approved: false,
            mergeable: false,
        });
    }

    let raw = String::from_utf8_lossy(&out.stdout);
    Ok(parse_gh_pr_view(&raw, &branch))
}

pub fn ci_ready_for_merge(report: &PrStatusReport) -> bool {
    report.state == PrState::Open
        && report.checks_passing
        && !report.checks_pending
        && report.review_approved
        && report.mergeable
}

pub fn merge_pr(project_dir: &Path, squash: bool) -> Result<String> {
    if !gh_available() {
        anyhow::bail!("未找到 gh CLI");
    }
    let cwd = lead_git_cwd(project_dir);
    let mut cmd = Command::new("gh");
    cmd.current_dir(&cwd);
    cmd.args(["pr", "merge", "--auto"]);
    if squash {
        cmd.arg("--squash");
    }
    let out = cmd.output().context("gh pr merge")?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    anyhow::bail!(
        "gh pr merge 失败: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

/// CI + review 通过后自动 `gh pr merge`（`AGENT_TUI_AUTO_MERGE=1`）。
pub fn maybe_auto_merge(project_dir: &Path, lead: &str) -> Result<bool> {
    if !auto_merge_enabled() {
        return Ok(false);
    }
    let merge = merge_ready::evaluate(project_dir);
    if !merge.ready || merge.done_tasks.is_empty() {
        return Ok(false);
    }
    if !should_poll_batch(project_dir, &merge.done_tasks) {
        return Ok(false);
    }

    let fp = merge_ready::done_tasks_fingerprint(&merge.done_tasks);
    if already_auto_merged(project_dir, &fp) {
        return Ok(false);
    }

    let status = query_pr_status(project_dir)?;
    if !ci_ready_for_merge(&status) {
        return Ok(false);
    }

    if ci_pass_simulated(project_dir) {
        simulate_merged(project_dir)?;
    } else {
        merge_pr(project_dir, auto_merge_squash())?;
    }

    remember_auto_merged(project_dir, &fp)?;
    let body = format!(
        "【auto-merge】CI/review 已通过，已合并 PR（{}）。\n\
         ▶ TUI 将轮询并派 post-github-merge 验证。",
        status.branch
    );
    meta::notify_agent_from(project_dir, lead, &body, "agent-tui")?;
    terminal::log_message(
        project_dir,
        "info",
        &format!("auto-merge: 已合并 PR（{}）", status.branch),
    );
    Ok(true)
}

fn post_github_checklist(done_tasks: &[String]) -> String {
    let list = done_tasks.join(", ");
    format!(
        "【GitHub 合并后验证】PR 已 merge 到主分支，批次 {list}：\n\
         1. `git pull` 更新主分支 / worktree\n\
         2. 跑全量测试 + 构建（与 CI 一致）\n\
         3. 确认无回归、无配置遗漏\n\
         4. 通过 → agent-report done；失败 → failed 并列阻塞项"
    )
}

pub fn dispatch_post_github_merge(
    project_dir: &Path,
    lead: &str,
    done_tasks: &[String],
) -> Result<bool> {
    let fp = merge_ready::done_tasks_fingerprint(done_tasks);
    let task_id = post_github_merge_task_id(done_tasks);
    if post_github_merge_passed(project_dir, done_tasks) {
        return Ok(false);
    }
    if already_dispatched(project_dir, &fp) {
        let snap = task_state::load_snapshots(project_dir);
        let st = snap
            .get(&task_id)
            .map(|s| s.status.as_str())
            .unwrap_or("missing");
        return Ok(st != "done" && st != "failed");
    }
    let reviewer = pick_reviewer(project_dir, lead).unwrap_or_else(|| "mimo".into());
    let desc = post_github_checklist(done_tasks);
    delegation::delegate_task(project_dir, lead, &reviewer, &task_id, &desc)?;
    remember_dispatched(project_dir, &fp)?;
    terminal::log_message(
        project_dir,
        "info",
        &format!("post-github-merge: 已委派 {reviewer}/{task_id}"),
    );
    Ok(true)
}

fn should_poll_batch(project_dir: &Path, done_tasks: &[String]) -> bool {
    if done_tasks.is_empty() {
        return false;
    }
    if !merge_ready::was_notified_for_done_tasks(project_dir, done_tasks) {
        return false;
    }
    if !post_merge_smoke::smoke_gate_satisfied(project_dir, done_tasks) {
        return false;
    }
    auto_pr::was_auto_pr_dispatched(project_dir, done_tasks)
        || pr_merged_simulated(project_dir)
        || query_pr_status(project_dir)
            .map(|r| matches!(r.state, PrState::Open | PrState::Merged))
            .unwrap_or(false)
}

/// Poll gh (or simulate) for merged PR → notify Lead + dispatch post-merge verification.
pub fn maybe_poll_merged(project_dir: &Path, lead: &str) -> Result<bool> {
    if !poll_pr_enabled() {
        return Ok(false);
    }
    let merge = merge_ready::evaluate(project_dir);
    if !merge.ready || merge.done_tasks.is_empty() {
        return Ok(false);
    }
    if !should_poll_batch(project_dir, &merge.done_tasks) {
        return Ok(false);
    }
    if post_github_merge_passed(project_dir, &merge.done_tasks) {
        return Ok(false);
    }

    let status = query_pr_status(project_dir)?;
    if status.state != PrState::Merged {
        return Ok(false);
    }

    let fp = merge_ready::done_tasks_fingerprint(&merge.done_tasks);
    if !already_notified_merged(project_dir, &fp) {
        let body = format!(
            "【pr-merged】PR 已合并到主分支（{}）。\n\
             ▶ TUI 已自动派 post-github-merge 验证 task。",
            status.branch
        );
        meta::notify_agent_from(project_dir, lead, &body, "agent-tui")?;
        remember_notified_merged(project_dir, &fp)?;
        terminal::log_message(
            project_dir,
            "info",
            &format!("pr-merged: 已通知 {lead}（{}）", status.branch),
        );
    }

    dispatch_post_github_merge(project_dir, lead, &merge.done_tasks)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pr-life-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        dir
    }

    #[test]
    fn simulate_merged_file() {
        let dir = fresh_dir("sim");
        assert!(!pr_merged_simulated(&dir));
        simulate_merged(&dir).unwrap();
        assert!(pr_merged_simulated(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ci_ready_parsing() {
        let raw = r#"{
            "state":"OPEN",
            "url":"https://github.com/o/r/pull/1",
            "reviewDecision":"APPROVED",
            "mergeStateStatus":"CLEAN",
            "statusCheckRollup":[{"state":"SUCCESS"}]
        }"#;
        let r = parse_gh_pr_view(raw, "feat-x");
        assert_eq!(r.state, PrState::Open);
        assert!(ci_ready_for_merge(&r));
    }
}
