use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Result};

use crate::agent_strengths;
use crate::claims;
use crate::config;
use crate::meta::{self, validate_task_name};
use crate::report_gate;
use crate::review_gate;
use crate::task_dag;
use crate::terminal;

fn delegate_hint_path(project_dir: &Path) -> std::path::PathBuf {
    project_dir.join(".agents/shared/delegate_hints.jsonl")
}

fn already_hinted(project_dir: &Path, key: &str) -> bool {
    let path = delegate_hint_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    content.lines().any(|l| l.trim() == key)
}

fn remember_hint(project_dir: &Path, key: &str) -> Result<()> {
    let path = delegate_hint_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{key}")?;
    Ok(())
}

/// Lead assigns a task to a worker: claim + notify + relay.
pub fn delegate_task(
    project_dir: &Path,
    lead: &str,
    worker: &str,
    task: &str,
    description: &str,
) -> Result<()> {
    if lead == worker {
        bail!("不能委派给自己");
    }
    let task = task.trim();
    validate_task_name(task)?;
    if let Ok(agents) = config::load_agents(project_dir) {
        if let Some(hint) =
            agent_strengths::delegation_mismatch(&agents, lead, worker, task, description, Some(project_dir))
        {
            terminal::log_message(project_dir, "info", &hint);
            let hint_key = format!("{task}:{worker}");
            if !already_hinted(project_dir, &hint_key) {
                let _ = meta::notify_agent_from(project_dir, lead, &hint, "agent-tui");
                let _ = remember_hint(project_dir, &hint_key);
            }
        }
    }
    claims::claim_task(project_dir, worker, task)?;

    let body = if description.trim().is_empty() {
        format!(
            "【委派·{task}】请立即开始执行。完成后输出 agent-report 代码块（系统自动回传主 Agent）：\
             ```agent-report {{\"task\":\"{task}\",\"status\":\"done\",\"summary\":\"完成了什么\"}} ``` \
             若 blocked 则 status=blocked 并说明原因。"
        )
    } else {
        format!(
            "【委派·{task}】请立即开始：{desc}。完成后输出：\
             ```agent-report {{\"task\":\"{task}\",\"status\":\"done\",\"summary\":\"…\"}} ```",
            desc = description.trim()
        )
    };
    meta::notify_agent_from(project_dir, worker, &body, lead)?;
    meta::append_shared_line(
        project_dir,
        &format!("{lead} 委派 @{worker} 任务「{task}」"),
    )?;
    if team_awareness_enabled() {
        let ping = format!("{lead} 已委派 {worker} 执行任务「{task}」，请知悉");
        if let Ok(agents) = config::load_agents(project_dir) {
            for spec in &agents {
                if spec.name == lead || spec.name == worker {
                    continue;
                }
                let _ = meta::append_agent_inbox_notice(project_dir, &spec.name, &ping, lead);
            }
        }
    }
    let _ = crate::agent_memory::on_delegate(project_dir, lead, worker, task, description);
    let _ = crate::mailbox::delegate(project_dir, lead, worker, task, description);
    let _ = crate::task_state::on_delegate(project_dir, lead, worker, task, description);
    Ok(())
}

fn team_awareness_enabled() -> bool {
    std::env::var("AGENT_TUI_TEAM_AWARENESS")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// Worker reports back to the lead agent.
pub fn report_task(
    project_dir: &Path,
    reporter: &str,
    lead: &str,
    task: Option<&str>,
    message: &str,
) -> Result<()> {
    let message = message.trim();
    if message.is_empty() {
        bail!("回执内容不能为空");
    }
    if reporter == lead {
        bail!("不能向自己回执");
    }
    let body = match task.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => validate_task_name(t).map(|_| format!("【回执·{t}】{message}"))?,
        None => format!("【回执】{message}"),
    };
    meta::notify_agent_from(project_dir, lead, &body, reporter)?;
    Ok(())
}

/// Worker report collected automatically from PTY transcript (phase-1 closed loop).
pub fn report_task_auto(
    project_dir: &Path,
    reporter: &str,
    lead: &str,
    task: &str,
    status: &str,
    summary: &str,
) -> Result<()> {
    if reporter == lead {
        bail!("不能向自己回执");
    }
    validate_task_name(task)?;
    let gate = report_gate::apply_report_gate(project_dir, reporter, task, status, summary)?;
    let mut status = gate.status;
    let mut summary = gate.summary;

    if status == "done" {
        if review_gate::is_review_task(task) {
            if let Some(parent) = review_gate::promote_parent_after_review(
                project_dir,
                lead,
                reporter,
                task,
                "done",
                &summary,
            ) {
                let _ = dispatch_pending_plans(project_dir, lead);
                let _ = crate::merge_ready::notify_lead_if_ready(project_dir, lead);
                let _ = parent;
            }
        } else {
            let review = review_gate::apply_implementation_done(
                project_dir,
                lead,
                reporter,
                task,
                &summary,
            );
            status = review.status;
            summary = review.summary;
            if let Some((reviewer, review_id, desc)) = review.delegate_review {
                if delegate_task(project_dir, lead, &reviewer, &review_id, &desc).is_ok() {
                    terminal::log_message(
                        project_dir,
                        "info",
                        &format!("review gate: 已委派 {reviewer}/{review_id}"),
                    );
                }
            }
        }
    } else if status == "failed" && review_gate::is_review_task(task) {
        let _ = review_gate::promote_parent_after_review(
            project_dir,
            lead,
            reporter,
            task,
            "failed",
            &summary,
        );
    }

    let status = status.as_str();
    let summary = {
        let s = summary.trim();
        if s.is_empty() {
            match status {
                "blocked" => "任务受阻，需要主 Agent 决策".to_string(),
                "failed" => "任务失败".to_string(),
                "awaiting_review" => "待审查".to_string(),
                _ => "已完成".to_string(),
            }
        } else {
            s.to_string()
        }
    };
    let body = format!(
        "【回执·{task}·{status}】{reporter} 完成。{summary}（TUI 自动采集）\
         ▶ Lead 行动：立即输出 ```agent-plan```（review {task} / 续派 / 修复），禁止询问用户是否继续。\
         规则：.cursor/rules/agent-tui-orchestrator.mdc"
    );
    meta::notify_agent_from(project_dir, lead, &body, reporter)?;
    meta::append_shared_line(
        project_dir,
        &format!("{reporter} → {lead} 回执「{task}」({status})"),
    )?;
    if status == "done" {
        let _ = claims::release_task(project_dir, reporter, task);
        let _ = task_dag::mark_task_completed(project_dir, task);
        dispatch_pending_plans(project_dir, lead);
    }
    let desc = crate::task_state::load_snapshots(project_dir)
        .get(task)
        .and_then(|s| s.summary.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| summary.to_string());
    let _ = crate::agent_memory::on_report(project_dir, reporter, lead, task, status, &summary);
    let _ = crate::task_state::on_report(project_dir, reporter, lead, task, status, &summary);
    let _ = crate::mailbox::report(project_dir, reporter, lead, task, status, &summary);
    let _ = crate::dead_letter::record_and_maybe_retry(
        project_dir,
        lead,
        reporter,
        task,
        status,
        &summary,
        &desc,
    );
    let _ = crate::delegation_stats::record_outcome(
        project_dir,
        reporter,
        task,
        status,
        &desc,
        lead,
        &summary,
    );
    let _ = crate::delegation_stats::maybe_auto_evolve(project_dir);
    if status == "done" || status == "failed" || status == "blocked" {
        let _ = crate::lead_followup::on_worker_report(project_dir, reporter, task, status);
    }
    Ok(())
}

/// Dispatch pending agent-plan items whose dependencies are now satisfied.
pub fn dispatch_pending_plans(project_dir: &Path, lead: &str) -> usize {
    let completed = task_dag::load_completed_tasks(project_dir);
    let ready = match task_dag::flush_pending_after_complete(project_dir, &completed) {
        Ok(r) => r,
        Err(e) => {
            terminal::log_message(
                project_dir,
                "warn",
                &format!("flush pending plans failed: {e:#}"),
            );
            return 0;
        }
    };
    let mut n = 0usize;
    for item in ready {
        if item.worker.eq_ignore_ascii_case(lead) {
            continue;
        }
        match delegate_task(
            project_dir,
            lead,
            &item.worker,
            &item.task,
            &item.description,
        ) {
            Ok(()) => n += 1,
            Err(e) => {
                terminal::log_message(
                    project_dir,
                    "warn",
                    &format!(
                        "pending dispatch {}/{} failed: {e:#}",
                        item.worker, item.task
                    ),
                );
            }
        }
    }
    if n > 0 {
        terminal::log_message(
            project_dir,
            "info",
            &format!("依赖满足后自动派发 {n} 个待办子任务"),
        );
    }
    n
}
