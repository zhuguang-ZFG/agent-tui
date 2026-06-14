//! Headless closed-loop verification (no TUI / no live PTY).

use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::agent_memory;
use crate::config::{load_agents, resolve_lead_agent};
use crate::dead_letter;
use crate::delegation;
use crate::lead_followup;
use crate::lead_watch;
use crate::mailbox;
use crate::meta;
use crate::observer;
use crate::report_watch;
use crate::relay;
use crate::task_dag;
use crate::task_state;

pub struct VerifyOutcome {
    pub task: String,
    pub plan_parsed: usize,
    pub delegated: bool,
    pub report_parsed: usize,
    pub reported: bool,
    pub memory_ok: bool,
}

pub struct RetryVerifyOutcome {
    pub task: String,
    pub failed_worker: String,
    pub retry_worker: String,
    pub dead_letter_attempt: u32,
    pub retries_executed: usize,
}

pub struct DagVerifyOutcome {
    pub dep_task: String,
    pub follow_task: String,
}

pub fn unique_task(prefix: &str) -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{prefix}-{ms}")
}

/// Parse-only checks (no filesystem writes).
pub fn verify_parsers() -> Result<()> {
    let names = vec![
        "cursor".into(),
        "codex".into(),
        "kimi".into(),
        "mimo".into(),
    ];
    let plan_text = r#"
```agent-plan
[{"worker":"codex","task":"parser-check","description":"x"}]
```
"#;
    let mut seen = HashSet::new();
    let plans = lead_watch::plans_from_text(plan_text, &names, &mut seen);
    if plans.len() != 1 {
        bail!("plan parser: expected 1 item, got {}", plans.len());
    }

    let report_text = r#"
```agent-report
{"task":"parser-check","status":"done","summary":"ok"}
```
"#;
    let mut seen_r = HashSet::new();
    let reports = report_watch::reports_from_text("codex", report_text, &mut seen_r);
    if reports.len() != 1 || reports[0].status != "done" {
        bail!("report parser: unexpected {:?}", reports);
    }

    let event = meta::CoordEvent {
        line_no: 1,
        time: None,
        kind: "notify".into(),
        agent: Some("codex".into()),
        message: "【委派·t】请开始".into(),
        from: "cursor".into(),
        task: None,
        action: None,
    };
    if !relay::is_delegate_notify(&event) {
        bail!("delegate notify detector failed");
    }
    Ok(())
}

/// Full delegate → report chain against a real project dir; writes events.jsonl.
pub fn verify_events_chain(project_dir: &Path) -> Result<VerifyOutcome> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let task = unique_task("loop-verify");
    let plan_text = format!(
        r#"
```agent-plan
[{{"worker":"codex","task":"{task}","description":"闭环验证任务"}}]
```
"#
    );

    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    let mut seen = HashSet::new();
    let plans = lead_watch::plans_from_text(&plan_text, &names, &mut seen);
    if plans.len() != 1 {
        bail!("expected 1 plan item, got {}", plans.len());
    }

    let before = meta::load_coord_events(project_dir).len();

    delegation::delegate_task(
        project_dir,
        &lead,
        "codex",
        &task,
        "闭环验证：echo ok",
    )
    .context("delegate_task")?;

    let report_text = format!(
        r#"
```agent-report
{{"task":"{task}","status":"done","summary":"闭环验证完成"}}
```
"#
    );
    let mut seen_r = HashSet::new();
    let reports = report_watch::reports_from_text("codex", &report_text, &mut seen_r);
    if reports.len() != 1 {
        bail!("expected 1 report, got {}", reports.len());
    }
    let r = &reports[0];
    delegation::report_task_auto(
        project_dir,
        "codex",
        &lead,
        &r.task,
        &r.status,
        &r.summary,
    )
    .context("report_task_auto")?;

    let events = meta::load_coord_events(project_dir);
    let after = events.len();
    if after <= before {
        bail!("events.jsonl did not grow (before={before}, after={after})");
    }

    let tail: Vec<_> = events.iter().rev().take(12).collect();
    let has_claim = tail.iter().any(|e| {
        e.kind == "claim"
            && e.task.as_deref() == Some(task.as_str())
            && e.agent.as_deref() == Some("codex")
    });
    let has_delegate_notify = tail.iter().any(|e| {
        e.kind == "notify"
            && e.agent.as_deref() == Some("codex")
            && e.from == lead
            && e.message.contains("【委派·")
            && e.message.contains(&task)
    });
    let has_report_notify = tail.iter().any(|e| {
        e.kind == "notify"
            && e.agent.as_deref() == Some(lead.as_str())
            && e.from == "codex"
            && e.message.contains("【回执·")
            && e.message.contains(&task)
    });

    if !has_claim {
        bail!("missing claim event for {task}");
    }
    if !has_delegate_notify {
        bail!("missing delegate notify to codex for {task}");
    }
    if !has_report_notify {
        bail!("missing report notify to {lead} for {task}");
    }

    verify_memory_artifacts(project_dir, &lead, "codex", &task)?;
    verify_mailbox_and_state(project_dir, &lead, &task)?;

    Ok(VerifyOutcome {
        task,
        plan_parsed: plans.len(),
        delegated: true,
        report_parsed: reports.len(),
        reported: true,
        memory_ok: true,
    })
}

fn verify_mailbox_and_state(project_dir: &Path, lead: &str, task: &str) -> Result<()> {
    let entries = mailbox::load_entries(project_dir, 30);
    let has_delegate = entries.iter().any(|e| {
        e.kind == "delegate" && e.task.as_deref() == Some(task) && e.from == lead
    });
    let has_report = entries.iter().any(|e| {
        e.kind == "report" && e.task.as_deref() == Some(task) && e.to == lead
    });
    if !has_delegate {
        bail!("mailbox missing delegate for {task}");
    }
    if !has_report {
        bail!("mailbox missing report for {task}");
    }
    let snap = task_state::load_snapshots(project_dir);
    match snap.get(task).map(|s| s.status.as_str()) {
        Some("done") => Ok(()),
        other => bail!("task_state expected done for {task}, got {other:?}"),
    }
}

fn verify_memory_artifacts(
    project_dir: &Path,
    lead: &str,
    worker: &str,
    task: &str,
) -> Result<()> {
    for agent in [worker, lead] {
        let cp = agent_memory::checkpoint_path(project_dir, agent);
        let body = std::fs::read_to_string(&cp)
            .with_context(|| format!("read checkpoint {}", cp.display()))?;
        if !body.contains(task) {
            bail!("checkpoint {} missing task {task}", cp.display());
        }
        if !body.contains("§1 Active intent") {
            bail!("checkpoint {} missing §1 section", cp.display());
        }

        let mem = agent_memory::memory_md_path(project_dir, agent);
        let mem_body = std::fs::read_to_string(&mem)
            .with_context(|| format!("read MEMORY {}", mem.display()))?;
        if !mem_body.contains(task) {
            bail!("MEMORY {} missing task {task}", mem.display());
        }

        let notes = agent_memory::notes_path(project_dir, agent);
        let notes_body = std::fs::read_to_string(&notes)
            .with_context(|| format!("read notes {}", notes.display()))?;
        if !notes_body.contains(task) {
            bail!("notes {} missing task {task}", notes.display());
        }
    }

    let mem_worker = std::fs::read_to_string(agent_memory::memory_md_path(project_dir, worker))?;
    if !mem_worker.contains(&format!("| {task} | done | codex |")) {
        bail!("worker MEMORY missing done assignment row for codex");
    }

    let mem_lead = std::fs::read_to_string(agent_memory::memory_md_path(project_dir, lead))?;
    if !mem_lead.contains(&format!("| {task} | done | codex |")) {
        bail!("lead MEMORY missing done assignment row pointing to codex");
    }

    Ok(())
}

/// failed report → dead_letter → retry_queue → re-delegate (with worker rotation).
pub fn verify_failed_retry_chain(project_dir: &Path) -> Result<RetryVerifyOutcome> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let failed_worker = "codex";
    if !agents.iter().any(|a| a.name == failed_worker) {
        bail!("verify retry: agents.yaml missing codex");
    }
    let expected_retry_worker =
        dead_letter::pick_retry_worker(project_dir, &lead, failed_worker, 1);

    let task = unique_task("retry-verify");
    delegation::delegate_task(
        project_dir,
        &lead,
        failed_worker,
        &task,
        "重试验证：模拟首次失败",
    )
    .context("initial delegate")?;

    delegation::report_task_auto(
        project_dir,
        failed_worker,
        &lead,
        &task,
        "failed",
        "模拟失败：verify-loop",
    )
    .context("failed report")?;

    let dead = dead_letter::load_dead_letters(project_dir, 20);
    let dl = dead
        .iter()
        .find(|r| r.task == task && r.worker == failed_worker && r.status == "failed")
        .ok_or_else(|| anyhow::anyhow!("missing dead_letter for {task}"))?;
    if dl.attempt != 1 {
        bail!("dead_letter attempt expected 1, got {}", dl.attempt);
    }

    let snap = task_state::load_snapshots(project_dir);
    match snap.get(&task).map(|s| s.status.as_str()) {
        Some("failed") => {}
        other => bail!("task_state expected failed after report, got {other:?}"),
    }

    if dead_letter::pending_retry_count(project_dir, &task) == 0 {
        bail!("retry_queue missing entry for {task}");
    }
    let queue = dead_letter::load_retry_queue(project_dir, 20);
    let pending = queue
        .iter()
        .find(|r| r.task == task)
        .ok_or_else(|| anyhow::anyhow!("retry_queue parse failed for {task}"))?;
    if pending.worker != expected_retry_worker {
        bail!(
            "retry worker expected {expected_retry_worker}, got {}",
            pending.worker
        );
    }
    if dead_letter::retry_rotate_enabled() && agents.len() > 2 {
        if pending.worker == failed_worker {
            bail!("worker rotation enabled but retry targets same worker");
        }
        if pending.previous_worker.as_deref() != Some(failed_worker) {
            bail!(
                "retry previous_worker expected {failed_worker}, got {:?}",
                pending.previous_worker
            );
        }
    }
    if dead_letter::retry_backoff() == dead_letter::RetryBackoff::Exponential {
        let cooldown = pending.cooldown_secs.unwrap_or(0);
        let expected = dead_letter::compute_cooldown_secs(1);
        if cooldown != expected {
            bail!("retry cooldown expected {expected}, got {cooldown}");
        }
    }

    let retries = dead_letter::process_retry_queue_immediate(project_dir, &lead);
    if retries == 0 {
        bail!("process_retry_queue_immediate executed 0 retries");
    }
    if dead_letter::pending_retry_count(project_dir, &task) != 0 {
        bail!("retry_queue still has pending entry after immediate process");
    }

    let entries = mailbox::load_entries(project_dir, 50);
    let delegate_workers: Vec<&str> = entries
        .iter()
        .filter(|e| e.kind == "delegate" && e.task.as_deref() == Some(task.as_str()))
        .map(|e| e.to.as_str())
        .collect();
    if !delegate_workers.iter().any(|w| *w == failed_worker) {
        bail!("mailbox missing initial delegate to {failed_worker}");
    }
    if !delegate_workers.iter().any(|w| *w == expected_retry_worker.as_str()) {
        bail!(
            "mailbox missing retry delegate to {expected_retry_worker}, got {delegate_workers:?}"
        );
    }

    delegation::report_task_auto(
        project_dir,
        &expected_retry_worker,
        &lead,
        &task,
        "done",
        "重试后验证完成",
    )
    .context("retry success report")?;

    match task_state::load_snapshots(project_dir)
        .get(&task)
        .map(|s| s.status.as_str())
    {
        Some("done") => {}
        other => bail!("task_state expected done after retry, got {other:?}"),
    }

    Ok(RetryVerifyOutcome {
        task,
        failed_worker: failed_worker.into(),
        retry_worker: expected_retry_worker,
        dead_letter_attempt: dl.attempt,
        retries_executed: retries,
    })
}

/// depends_on: follow task stays pending until dep task is done, then auto-delegates.
pub fn verify_dag_chain(project_dir: &Path) -> Result<DagVerifyOutcome> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    if !agents.iter().any(|a| a.name == "codex") || !agents.iter().any(|a| a.name == "kimi") {
        bail!("verify DAG: need codex + kimi in agents.yaml");
    }

    let dep_task = unique_task("dag-dep");
    let follow_task = unique_task("dag-ui");
    let items = vec![
        lead_watch::PlanItem {
            worker: "codex".into(),
            task: dep_task.clone(),
            description: "DAG 验证：前置 API".into(),
            depends_on: vec![],
        },
        lead_watch::PlanItem {
            worker: "kimi".into(),
            task: follow_task.clone(),
            description: "DAG 验证：依赖前置的 UI".into(),
            depends_on: vec![dep_task.clone()],
        },
    ];

    let dispatched = lead_watch::dispatch_plan_items(project_dir, &lead, items, "verify-dag");
    if dispatched != 1 {
        bail!("DAG: expected 1 immediate dispatch, got {dispatched}");
    }

    let pending = task_dag::load_pending_plans(project_dir);
    if !pending.iter().any(|p| p.task == follow_task) {
        bail!("DAG: {follow_task} should be in pending_plans");
    }

    let entries = mailbox::load_entries(project_dir, 80);
    let kimi_delegated = entries.iter().any(|e| {
        e.kind == "delegate"
            && e.to == "kimi"
            && e.task.as_deref() == Some(follow_task.as_str())
    });
    if kimi_delegated {
        bail!("DAG: kimi should not be delegated before dep completes");
    }

    delegation::report_task_auto(
        project_dir,
        "codex",
        &lead,
        &dep_task,
        "done",
        "DAG 前置完成",
    )
    .context("dep done report")?;

    let pending_after = task_dag::load_pending_plans(project_dir);
    if pending_after.iter().any(|p| p.task == follow_task) {
        bail!("DAG: {follow_task} still pending after dep done");
    }

    let entries2 = mailbox::load_entries(project_dir, 80);
    if !entries2.iter().any(|e| {
        e.kind == "delegate"
            && e.to == "kimi"
            && e.task.as_deref() == Some(follow_task.as_str())
    }) {
        bail!("DAG: missing kimi delegate after dep done");
    }

    match task_state::load_snapshots(project_dir)
        .get(&follow_task)
        .map(|s| s.status.as_str())
    {
        Some("delegated") => {}
        other => bail!("DAG: follow task expected delegated, got {other:?}"),
    }

    Ok(DagVerifyOutcome {
        dep_task,
        follow_task,
    })
}

/// Worker done → lead followup pending → lead plan dispatch clears pending.
pub fn verify_lead_followup_chain(project_dir: &Path) -> Result<()> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let task1 = unique_task("follow-dep");

    delegation::delegate_task(
        project_dir,
        &lead,
        "codex",
        &task1,
        "续派验证：前置任务",
    )?;
    delegation::report_task_auto(
        project_dir,
        "codex",
        &lead,
        &task1,
        "done",
        "前置完成，等待主 Agent 续派",
    )?;

    if lead_followup::pending_count(project_dir) == 0 {
        bail!("lead followup: expected pending after done report");
    }

    let task2 = unique_task("follow-next");
    let items = vec![lead_watch::PlanItem {
        worker: "mimo".into(),
        task: task2.clone(),
        description: "续派验证：第二波任务".into(),
        depends_on: vec![],
    }];
    let n = lead_watch::dispatch_plan_items(project_dir, &lead, items, "verify-followup");
    if n != 1 {
        bail!("lead followup: expected 1 dispatch for follow-up plan, got {n}");
    }

    if lead_followup::pending_count(project_dir) != 0 {
        bail!("lead followup: pending should clear after plan dispatch");
    }

    if !lead_followup::load_records(project_dir, 20)
        .iter()
        .any(|r| r.task == task1 && r.satisfied)
    {
        bail!("lead followup: record for {task1} should be satisfied");
    }

    Ok(())
}

/// Lead PTY transcript tail → parse agent-plan → auto-dispatch → clear followup pending.
pub fn verify_lead_transcript_followup_chain(project_dir: &Path) -> Result<()> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    let task1 = unique_task("tx-follow-dep");

    delegation::delegate_task(
        project_dir,
        &lead,
        "codex",
        &task1,
        "transcript 续派验证",
    )?;
    delegation::report_task_auto(
        project_dir,
        "codex",
        &lead,
        &task1,
        "done",
        "完成，等待 cursor transcript 续派",
    )?;

    if lead_followup::pending_count(project_dir) == 0 {
        bail!("transcript followup: expected pending after report");
    }

    let task2 = unique_task("tx-follow-next");
    let plan_text = format!(
        r#"
```agent-plan
[{{"worker":"mimo","task":"{task2}","description":"transcript 自动扫描续派"}}]
```
"#
    );

    let mut lead_watch = lead_watch::LeadWatchState::new();
    let outcome = lead_followup::process_transcript_text(
        project_dir,
        &lead,
        &names,
        &plan_text,
        &mut lead_watch,
    );
    if outcome.dispatched != 1 {
        bail!(
            "transcript followup: expected 1 dispatch, got {}",
            outcome.dispatched
        );
    }
    if lead_followup::pending_count(project_dir) != 0 {
        bail!("transcript followup: pending should clear after dispatch");
    }

    let entries = mailbox::load_entries(project_dir, 50);
    if !entries.iter().any(|e| {
        e.kind == "delegate"
            && e.to == "mimo"
            && e.task.as_deref() == Some(task2.as_str())
    }) {
        bail!("transcript followup: missing mimo delegate in mailbox");
    }

    Ok(())
}

pub fn run_all(project_dir: &Path) -> Result<()> {
    verify_parsers().context("parser checks")?;
    let outcome = verify_events_chain(project_dir).context("events chain")?;
    let retry_outcome = verify_failed_retry_chain(project_dir).context("failed→retry chain")?;
    let dag_outcome = verify_dag_chain(project_dir).context("DAG chain")?;
    verify_lead_followup_chain(project_dir).context("lead followup chain")?;
    verify_lead_transcript_followup_chain(project_dir).context("lead transcript followup")?;
    observer::verify_http_snapshot(project_dir).context("observer HTTP")?;
    observer::verify_sse_stream(project_dir).context("observer SSE")?;
    let snap = observer::build_snapshot(project_dir).context("observer snapshot")?;
    if !snap.tasks.iter().any(|t| t.task == outcome.task && t.status == "done") {
        anyhow::bail!("observer snapshot missing done task {}", outcome.task);
    }
    println!("闭环验证通过");
    println!("  任务 ID: {}", outcome.task);
    println!("  agent-plan 解析: {} 条", outcome.plan_parsed);
    println!("  委派 → codex: ok");
    println!("  agent-report 解析: {} 条", outcome.report_parsed);
    println!("  回执 → {}: ok", project_dir.display());
    println!("  events.jsonl: claim + delegate-notify + report-notify ✓");
    println!("  mailbox + task_state ✓");
    println!("  memory: checkpoint + MEMORY + notes（{} / codex）✓", {
        let agents = load_agents(project_dir)?;
        resolve_lead_agent(&agents)
    });
    println!("  failed→retry: {} failed@{} → retry@{} (attempt {}) ✓",
        retry_outcome.task,
        retry_outcome.failed_worker,
        retry_outcome.retry_worker,
        retry_outcome.dead_letter_attempt,
    );
    println!(
        "  DAG depends_on: {} → {} 延迟派发 ✓",
        dag_outcome.dep_task, dag_outcome.follow_task
    );
    println!("  lead followup: 回执 pending → 续派 plan 清除 ✓");
    println!("  lead transcript: tail 扫描 agent-plan → 自动派发 ✓");
    println!("  observer: /api/snapshot + SSE stream ✓");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers_ok() {
        verify_parsers().expect("parsers");
    }
}
