//! Single source of truth for「当前阶段 + 一条命令」— used by `next` CLI and TUI badge.

use std::path::Path;

use ratatui::style::Color;

use crate::batch_review;
use crate::merge_ready;
use crate::review_gate::is_meta_task;
use crate::verify_cleanup::is_verify_artifact_task;
use crate::task_state;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    InProgress,
    AwaitingReview,
    NeedsLead,
    AwaitingBatchReview,
    BatchReviewFailed,
    MergeReadyPending,
    MergeReady,
    PostMergeSmoke,
    PrCreated,
    PrMerged,
    PostGithubMerge,
    Shipped,
}

#[derive(Debug, Clone)]
pub struct WorkflowSnapshot {
    pub phase: Phase,
    pub title: String,
    pub detail: String,
    pub action: String,
    pub command: Option<String>,
}

pub fn evaluate(project_dir: &Path) -> WorkflowSnapshot {
    let dir_flag = format!(" --project-dir {}", project_dir.display());
    let impl_tasks = collect_impl_tasks(project_dir);

    if impl_tasks.is_empty() {
        // verify-loop 残留会让 next 误报；提示清理。
        let shared = project_dir.join(".agents/shared/task_state.jsonl");
        if shared.is_file() {
            let raw = std::fs::read_to_string(shared).unwrap_or_default();
            if raw.lines().any(|l| {
                l.contains("loop-verify-")
                    || l.contains("blocked-esc-")
                    || l.contains("dag-dep-")
                    || l.contains("retry-verify-")
            }) {
                return WorkflowSnapshot {
                    phase: Phase::Idle,
                    title: "空闲（有验证残留）".into(),
                    detail: "task_state 含 verify-loop 测试任务，非真实受阻".into(),
                    action: "运行 agent-tui clean-verify 清理后再看 next".into(),
                    command: Some(format!("agent-tui clean-verify{dir_flag}")),
                };
            }
        }
        return WorkflowSnapshot {
            phase: Phase::Idle,
            title: "空闲".into(),
            detail: "尚无任务进入协调状态机".into(),
            action: "启动 TUI，在留言板输入 !任务 或直接向 Lead 描述需求".into(),
            command: Some(format!("agent-tui{dir_flag}")),
        };
    }

    let mut failed = 0usize;
    let mut blocked = 0usize;
    let mut review_failed = 0usize;
    let mut awaiting_review = 0usize;
    let mut in_flight = 0usize;

    for (_, status) in &impl_tasks {
        match status.as_str() {
            "failed" => failed += 1,
            "blocked" => blocked += 1,
            "review_failed" => review_failed += 1,
            "awaiting_review" => awaiting_review += 1,
            "delegated" | "pending" => in_flight += 1,
            _ => {}
        }
    }

    if failed > 0 || blocked > 0 || review_failed > 0 {
        let parts = [
            (failed, "失败"),
            (blocked, "受阻"),
            (review_failed, "审查未过"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, label)| format!("{n} {label}"))
        .collect::<Vec<_>>()
        .join("，");
        return WorkflowSnapshot {
            phase: Phase::NeedsLead,
            title: "需 Lead 决策".into(),
            detail: format!("{parts} — 等 Lead 输出 agent-plan 修复或改派"),
            action: "保持 TUI 运行，聚焦 Lead（Ctrl+1）续派".into(),
            command: None,
        };
    }

    if awaiting_review > 0 {
        return WorkflowSnapshot {
            phase: Phase::AwaitingReview,
            title: "逐 task 审查中".into(),
            detail: format!("{awaiting_review} 个子任务待 reviewer 通过"),
            action: "TUI 已自动派 review task，无需操作".into(),
            command: None,
        };
    }

    if in_flight > 0 {
        return WorkflowSnapshot {
            phase: Phase::InProgress,
            title: "执行中".into(),
            detail: format!("{in_flight} 个子任务进行中"),
            action: "保持 TUI 运行，工人完成后自动回执与续派".into(),
            command: None,
        };
    }

    let merge = merge_ready::evaluate(project_dir);
    if !merge.ready {
        return WorkflowSnapshot {
            phase: Phase::InProgress,
            title: "执行中".into(),
            detail: format!("阻塞: {}", merge.blocking.join(", ")),
            action: "保持 TUI 运行".into(),
            command: None,
        };
    }

    let batch_id = batch_review::batch_review_task_id(&merge.done_tasks);
    if batch_review::batch_review_enabled() {
        match batch_review::batch_review_status(project_dir, &batch_id) {
            Some(st) if st == "done" => {}
            Some(st) if st == "failed" => {
                return WorkflowSnapshot {
                    phase: Phase::BatchReviewFailed,
                    title: "批次审查未通过".into(),
                    detail: format!("{batch_id} — Lead 派修复后重审"),
                    action: "Lead 输出 agent-plan 派发修复 task，完成后执行 review --force".into(),
                    command: Some(format!("agent-tui review --force{dir_flag}")),
                };
            }
            Some(st) => {
                return WorkflowSnapshot {
                    phase: Phase::AwaitingBatchReview,
                    title: "批次审查中".into(),
                    detail: format!("{batch_id}（{st}）"),
                    action: "等 reviewer 完成批次审查".into(),
                    command: None,
                };
            }
            None => {
                return WorkflowSnapshot {
                    phase: Phase::AwaitingBatchReview,
                    title: "等待批次审查".into(),
                    detail: "子任务已全部 done，TUI 将自动派 batch-review".into(),
                    action: "保持 TUI 运行即可".into(),
                    command: None,
                };
            }
        }
    }

    if merge_ready::was_notified_for_done_tasks(project_dir, &merge.done_tasks) {
        if crate::pr_lifecycle::post_github_merge_passed(project_dir, &merge.done_tasks) {
            return WorkflowSnapshot {
                phase: Phase::Shipped,
                title: "已交付".into(),
                detail: format!("批次 {} 已合并并验证通过", merge.done_tasks.join(", ")),
                action: "本批次闭环完成，可开始下一波 !任务".into(),
                command: None,
            };
        }
        if crate::pr_lifecycle::query_pr_status(project_dir)
            .map(|r| r.state == crate::pr_lifecycle::PrState::Merged)
            .unwrap_or(false)
        {
            let task_id = crate::pr_lifecycle::post_github_merge_task_id(&merge.done_tasks);
            if !crate::pr_lifecycle::post_github_merge_passed(project_dir, &merge.done_tasks) {
                return WorkflowSnapshot {
                    phase: Phase::PostGithubMerge,
                    title: "合并后验证".into(),
                    detail: format!("{task_id} — PR 已 merge，等 reviewer 验证主分支"),
                    action: "保持 TUI 运行，验证通过后批次闭环".into(),
                    command: None,
                };
            }
        }
        if crate::auto_pr::was_auto_pr_dispatched(project_dir, &merge.done_tasks)
            || crate::pr_lifecycle::was_auto_merge_dispatched(project_dir, &merge.done_tasks)
        {
            let pr = crate::pr_lifecycle::query_pr_status(project_dir).ok();
            if pr.as_ref().map(|r| r.state) == Some(crate::pr_lifecycle::PrState::Open) {
                let action = if crate::pr_lifecycle::auto_merge_enabled() {
                    if pr.as_ref().is_some_and(|r| r.checks_pending) {
                        "CI 运行中，通过后 AGENT_TUI_AUTO_MERGE=1 将自动 merge".into()
                    } else if pr.as_ref().is_some_and(|r| !r.checks_passing) {
                        "CI 未通过，修复后自动 merge".into()
                    } else if pr.as_ref().is_some_and(|r| !r.review_approved) {
                        "等待 review 批准，通过后自动 merge".into()
                    } else {
                        "CI/review 已通过，将自动 merge".into()
                    }
                } else {
                    "GitHub review 后 merge，或 agent-tui pr-merge".into()
                };
                return WorkflowSnapshot {
                    phase: Phase::PrCreated,
                    title: "PR 待合并".into(),
                    detail: pr
                        .map(|r| r.message)
                        .unwrap_or_else(|| "PR 已创建".into()),
                    action,
                    command: if crate::pr_lifecycle::auto_merge_enabled() {
                        None
                    } else {
                        Some(format!("agent-tui pr-merge{dir_flag}"))
                    },
                };
            }
            if pr.as_ref().map(|r| r.state) == Some(crate::pr_lifecycle::PrState::Merged) {
                return WorkflowSnapshot {
                    phase: Phase::PrMerged,
                    title: "PR 已合并".into(),
                    detail: "等待 post-github-merge 验证派发".into(),
                    action: "TUI 将自动派验证 task（AGENT_TUI_POLL_PR=1）".into(),
                    command: None,
                };
            }
            return WorkflowSnapshot {
                phase: Phase::PrCreated,
                title: "PR 已创建".into(),
                detail: "auto-pr 已执行；查询状态: agent-tui pr-status".into(),
                action: "等 GitHub review 后 merge".into(),
                command: Some(format!("agent-tui pr-status{dir_flag}")),
            };
        }
        if crate::post_merge_smoke::post_merge_smoke_enabled()
            && !crate::post_merge_smoke::smoke_passed(project_dir, &merge.done_tasks)
        {
            let smoke_id = crate::post_merge_smoke::smoke_task_id(&merge.done_tasks);
            return WorkflowSnapshot {
                phase: Phase::PostMergeSmoke,
                title: "合并前 smoke".into(),
                detail: format!("{smoke_id} — reviewer 跑测试/构建验证"),
                action: "等 smoke 通过后再 pr-create".into(),
                command: None,
            };
        }
        return WorkflowSnapshot {
            phase: Phase::MergeReady,
            title: "merge-ready".into(),
            detail: format!("已通过: {}", merge.done_tasks.join(", ")),
            action: if crate::auto_pr::auto_pr_enabled() {
                "已启用 AGENT_TUI_AUTO_PR=1，smoke 通过后自动开 PR".into()
            } else {
                "在 feature 分支开 PR（或 agents-complete merge）".into()
            },
            command: Some(if crate::auto_pr::auto_pr_enabled() {
                format!("（smoke 通过后自动）或 agent-tui pr-create{dir_flag}")
            } else {
                format!("agent-tui pr-create{dir_flag}")
            }),
        };
    }

    WorkflowSnapshot {
        phase: Phase::MergeReadyPending,
        title: "即将 merge-ready".into(),
        detail: "批次审查已通过，TUI 将通知 Lead".into(),
        action: "保持 TUI 运行，收到【merge-ready】后开 PR".into(),
        command: None,
    }
}

pub fn phase_id(phase: &Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::InProgress => "in_progress",
        Phase::AwaitingReview => "awaiting_review",
        Phase::NeedsLead => "needs_lead",
        Phase::AwaitingBatchReview => "batch_review",
        Phase::BatchReviewFailed => "batch_review_failed",
        Phase::MergeReadyPending => "merge_ready_pending",
        Phase::MergeReady => "merge_ready",
        Phase::PostMergeSmoke => "post_merge_smoke",
        Phase::PrCreated => "pr_created",
        Phase::PrMerged => "pr_merged",
        Phase::PostGithubMerge => "post_github_merge",
        Phase::Shipped => "shipped",
    }
}

pub fn short_label(project_dir: &Path) -> String {
    short_label_from(&evaluate(project_dir))
}

pub fn short_label_from(snapshot: &WorkflowSnapshot) -> String {
    let tag = phase_tag(&snapshot.phase);
    format!("阶段:{tag}")
}

fn phase_tag(phase: &Phase) -> &'static str {
    match phase {
        Phase::Idle => "空闲",
        Phase::InProgress => "执行中",
        Phase::AwaitingReview => "逐task审查",
        Phase::NeedsLead => "需Lead",
        Phase::AwaitingBatchReview => "批次审查",
        Phase::BatchReviewFailed => "批次未过",
        Phase::MergeReadyPending => "即将ready",
        Phase::MergeReady => "merge-ready",
        Phase::PostMergeSmoke => "smoke验证",
        Phase::PrCreated => "PR待合并",
        Phase::PrMerged => "PR已合并",
        Phase::PostGithubMerge => "合并后验证",
        Phase::Shipped => "已交付",
    }
}

/// Semantic color for status bar badge — glanceable workflow state.
pub fn badge_color(phase: &Phase) -> Color {
    match phase {
        Phase::Idle => Color::DarkGray,
        Phase::InProgress
        | Phase::AwaitingReview
        | Phase::MergeReadyPending
        | Phase::PostMergeSmoke
        | Phase::PostGithubMerge => Color::Cyan,
        Phase::NeedsLead | Phase::BatchReviewFailed => Color::Yellow,
        Phase::AwaitingBatchReview | Phase::MergeReady => Color::LightGreen,
        Phase::PrCreated => Color::Magenta,
        Phase::PrMerged | Phase::Shipped => Color::Green,
    }
}

pub fn action_hint(snapshot: &WorkflowSnapshot, max_chars: usize) -> String {
    let action = snapshot.action.trim();
    if action.is_empty() {
        return String::new();
    }
    if action.chars().count() <= max_chars {
        return action.to_string();
    }
    let tail: String = action
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect();
    format!("{tail}…")
}

pub fn format_next_report(snapshot: &WorkflowSnapshot) -> String {
    let mut out = format!("当前阶段: {}\n详情: {}\n", snapshot.title, snapshot.detail);
    out.push_str(&format!("你现在只需: {}\n", snapshot.action));
    match &snapshot.command {
        Some(cmd) => {
            out.push_str(&format!("命令: {cmd}\n"));
            if let Some(tui) = tui_shortcut_for_cli(cmd) {
                out.push_str(&format!("TUI: {tui}\n"));
            }
        }
        None => out.push_str("TUI: 保持运行，后台自动处理\n"),
    }
    out
}

fn tui_shortcut_for_cli(cmd: &str) -> Option<&'static str> {
    if cmd.contains("clean-verify") {
        Some("留言板 !clean")
    } else if cmd.contains("reset-batch") {
        Some("留言板 !reset-batch")
    } else if cmd.contains("pr-create") {
        Some("留言板 !pr")
    } else if cmd.contains("pr-merge") {
        Some("留言板 !pr-merge")
    } else if cmd.contains("pr-status") {
        Some("留言板 !pr-status")
    } else if cmd.contains("merge --all") || cmd.contains("merge-all") {
        Some("留言板 !merge-all")
    } else if cmd.contains("review --force") {
        Some("留言板 !review --force")
    } else if cmd.contains("review") {
        Some("留言板 !review")
    } else if cmd.contains("sync-lead") {
        Some("留言板 !sync-lead")
    } else {
        None
    }
}

fn collect_impl_tasks(project_dir: &Path) -> Vec<(String, String)> {
    let snap = task_state::load_snapshots(project_dir);
    let batch = merge_ready::merge_batch_prefix();
    let mut out = Vec::new();
    for (task, s) in snap {
        if is_meta_task(&task) || is_verify_artifact_task(&task) {
            continue;
        }
        if let Some(ref prefix) = batch {
            if !task.starts_with(prefix) {
                continue;
            }
        }
        if matches!(
            s.status.as_str(),
            "delegated" | "pending" | "awaiting_review" | "review_failed" | "blocked" | "failed"
                | "done"
        ) {
            out.push((task, s.status));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wf-phase-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        dir
    }

    #[test]
    fn idle_when_no_tasks() {
        let dir = fresh_dir("idle");
        let s = evaluate(&dir);
        assert_eq!(s.phase, Phase::Idle);
        assert!(s.command.as_ref().unwrap().contains("agent-tui"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_progress_when_delegated() {
        let dir = fresh_dir("prog");
        task_state::on_delegate(&dir, "cursor", "codex", "feat-a", "do it").unwrap();
        let s = evaluate(&dir);
        assert_eq!(s.phase, Phase::InProgress);
        assert!(s.command.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_ready_suggests_pr_create() {
        let dir = fresh_dir("mr");
        for key in [
            "AGENT_TUI_MERGE_BATCH",
            "AGENT_TUI_AUTO_PR",
            "AGENT_TUI_POLL_PR",
        ] {
            std::env::remove_var(key);
        }
        let task = format!("mr-{}", std::process::id());
        task_state::on_report(&dir, "codex", "cursor", &task, "done", "ok").unwrap();
        let done = merge_ready::evaluate(&dir).done_tasks;
        let fp = merge_ready::done_tasks_fingerprint(&done);
        let path = dir.join(".agents/shared/merge_ready_notified.jsonl");
        fs::write(&path, format!("{fp}\n")).unwrap();
        // Mark pre-PR smoke done so parallel tests cannot flip POST_MERGE_SMOKE env and flake this test.
        let smoke_id = crate::post_merge_smoke::smoke_task_id(&done);
        task_state::on_report(&dir, "mimo", "cursor", &smoke_id, "done", "ok").unwrap();
        std::env::set_var("AGENT_TUI_BATCH_REVIEW", "0");
        std::env::set_var("AGENT_TUI_POST_MERGE_SMOKE", "0");
        let s = evaluate(&dir);
        std::env::remove_var("AGENT_TUI_BATCH_REVIEW");
        std::env::remove_var("AGENT_TUI_POST_MERGE_SMOKE");
        assert_eq!(s.phase, Phase::MergeReady, "detail={}", s.detail);
        assert!(s.command.as_ref().unwrap().contains("pr-create"));
        let _ = fs::remove_dir_all(&dir);
    }
}
