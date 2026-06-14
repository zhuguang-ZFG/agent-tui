use std::path::Path;

use anyhow::{bail, Result};

use crate::claims;
use crate::config;
use crate::meta::{self, validate_task_name};
use crate::report_gate;
use crate::task_dag;
use crate::terminal;

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
    let status = gate.status.as_str();
    let summary = gate.summary.trim();
    let summary = if summary.is_empty() {
        match status {
            "blocked" => "任务受阻，需要主 Agent 决策",
            "failed" => "任务失败",
            _ => "已完成",
        }
    } else {
        summary
    };
    let body = format!(
        "【回执·{task}·{status}】{summary}（TUI 自动采集）\
         主 Agent：请立即输出下一波 agent-plan（review/修复/续派），勿等用户。\
         规则：.cursor/rules/agent-tui-orchestrator.mdc 或 AGENT_TUI_COORD_DOC"
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
    let _ = crate::agent_memory::on_report(project_dir, reporter, lead, task, status, summary);
    let _ = crate::task_state::on_report(project_dir, reporter, lead, task, status, summary);
    let _ = crate::mailbox::report(project_dir, reporter, lead, task, status, summary);
    let _ = crate::dead_letter::record_and_maybe_retry(
        project_dir,
        lead,
        reporter,
        task,
        status,
        summary,
        &desc,
    );
    let _ = crate::lead_followup::on_worker_report(project_dir, reporter, task, status);
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
