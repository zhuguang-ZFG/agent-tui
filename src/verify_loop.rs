//! Headless closed-loop verification (no TUI / no live PTY).

use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::agent_memory;
use crate::coord_dedupe::{self, RelayCursorState};
use crate::config::{load_agents, resolve_lead_agent};
use crate::dead_letter;
use crate::delegation;
use crate::lead_followup;
use crate::lead_identity;
use crate::lead_watch;
use crate::mailbox;
use crate::merge_ready;
use crate::meta;
use crate::observer;
use crate::report_watch;
use crate::project_init::{self, InitOptions};
use crate::relay::{self, RelayState};
use crate::review_gate;
use crate::task_dag;
use crate::task_state;

pub struct VerifyOutcome {
    pub task: String,
    pub plan_parsed: usize,
    #[allow(dead_code)]
    pub delegated: bool,
    pub report_parsed: usize,
    #[allow(dead_code)]
    pub reported: bool,
    #[allow(dead_code)]
    pub memory_ok: bool,
}

pub struct RetryVerifyOutcome {
    pub task: String,
    pub failed_worker: String,
    pub retry_worker: String,
    pub dead_letter_attempt: u32,
    #[allow(dead_code)]
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

/// First agent that is NOT the lead — used as the default test worker.
fn pick_non_lead(agents: &[crate::config::AgentSpec], lead: &str) -> String {
    agents
        .iter()
        .find(|a| !a.name.eq_ignore_ascii_case(lead))
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "claude".into())
}

fn without_review_gate<T, F: FnOnce() -> Result<T>>(f: F) -> Result<T> {
    std::env::set_var("AGENT_TUI_REVIEW_GATE", "0");
    let out = f();
    std::env::remove_var("AGENT_TUI_REVIEW_GATE");
    out
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
    let plans = lead_watch::plans_from_text(plan_text, &names, &mut seen, None);
    if plans.len() != 1 {
        bail!("plan parser: expected 1 item, got {}", plans.len());
    }

    let report_text = r#"
```agent-report
{"task":"parser-check","status":"done","summary":"ok"}
```
"#;
    let mut seen_r = HashSet::new();
    let reports = report_watch::reports_from_text("codex", report_text, &mut seen_r, None);
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
    let worker = pick_non_lead(&agents, &lead);
    let task = unique_task("loop-verify");
    let plan_text = format!(
        r#"
```agent-plan
[{{"worker":"{worker}","task":"{task}","description":"闭环验证任务"}}]
```
"#,
        worker = worker, task = task
    );

    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    let mut seen = HashSet::new();
    let plans = lead_watch::plans_from_text(&plan_text, &names, &mut seen, None);
    if plans.len() != 1 {
        bail!("expected 1 plan item, got {}", plans.len());
    }

    let before = meta::load_coord_events(project_dir).len();

    delegation::delegate_task(
        project_dir,
        &lead,
        &worker,
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
    let reports = report_watch::reports_from_text(&worker, &report_text, &mut seen_r, None);
    if reports.len() != 1 {
        bail!("expected 1 report, got {}", reports.len());
    }
    let r = &reports[0];
    delegation::report_task_auto(
        project_dir,
        &worker,
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
            && e.agent.as_deref() == Some(worker.as_str())
    });
    let has_delegate_notify = tail.iter().any(|e| {
        e.kind == "notify"
            && e.agent.as_deref() == Some(worker.as_str())
            && e.from == lead
            && e.message.contains("【委派·")
            && e.message.contains(&task)
    });
    let has_report_notify = tail.iter().any(|e| {
        e.kind == "notify"
            && e.agent.as_deref() == Some(lead.as_str())
            && e.from == worker
            && e.message.contains("【回执·")
            && e.message.contains(&task)
    });

    if !has_claim {
        bail!("missing claim event for {task}");
    }
    if !has_delegate_notify {
        bail!("missing delegate notify to {worker} for {task}");
    }
    if !has_report_notify {
        bail!("missing report notify to {lead} for {task}");
    }

    verify_memory_artifacts(project_dir, &lead, &worker, &task)?;
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
    if !mem_worker.contains(&format!("| {task} | done | {worker} |")) {
        bail!("worker MEMORY missing done assignment row for {worker}");
    }

    let mem_lead = std::fs::read_to_string(agent_memory::memory_md_path(project_dir, lead))?;
    if !mem_lead.contains(&format!("| {task} | done | {worker} |")) {
        bail!("lead MEMORY missing done assignment row pointing to {worker}");
    }

    Ok(())
}

/// failed report → dead_letter → retry_queue → re-delegate (with worker rotation).
pub fn verify_failed_retry_chain(project_dir: &Path) -> Result<RetryVerifyOutcome> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let failed_worker = pick_non_lead(&agents, &lead);
    if !agents.iter().any(|a| a.name == failed_worker) {
        bail!("verify retry: agents.yaml missing {failed_worker}");
    }
    let expected_retry_worker =
        dead_letter::pick_retry_worker(project_dir, &lead, &failed_worker, 1);

    let task = unique_task("retry-verify");
    delegation::delegate_task(
        project_dir,
        &lead,
        &failed_worker,
        &task,
        "重试验证：模拟首次失败",
    )
    .context("initial delegate")?;

    delegation::report_task_auto(
        project_dir,
        &failed_worker,
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
        if pending.previous_worker.as_deref() != Some(&failed_worker) {
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
    if !delegate_workers.contains(&failed_worker.as_str()) {
        bail!("mailbox missing initial delegate to {failed_worker}");
    }
    if !delegate_workers.contains(&expected_retry_worker.as_str()) {
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
    let worker_a = pick_non_lead(&agents, &lead);
    let worker_b = agents
        .iter()
        .find(|a| a.name != lead && a.name != worker_a)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "kimi".into());
    if !agents.iter().any(|a| a.name == worker_a) || !agents.iter().any(|a| a.name == worker_b) {
        bail!("verify DAG: need two non-lead agents in agents.yaml");
    }

    let dep_task = unique_task("dag-dep");
    let follow_task = unique_task("dag-ui");
    let items = vec![
        lead_watch::PlanItem {
            worker: worker_a.clone(),
            task: dep_task.clone(),
            description: "DAG 验证：前置 API".into(),
            depends_on: vec![],
        },
        lead_watch::PlanItem {
            worker: worker_b.clone(),
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
            && e.to == worker_b
            && e.task.as_deref() == Some(follow_task.as_str())
    });
    if kimi_delegated {
        bail!("DAG: {worker_b} should not be delegated before dep completes");
    }

    delegation::report_task_auto(
        project_dir,
        &worker_a,
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
            && e.to == worker_b
            && e.task.as_deref() == Some(follow_task.as_str())
    }) {
        bail!("DAG: missing {worker_b} delegate after dep done");
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
    let worker = pick_non_lead(&agents, &lead);
    let task1 = unique_task("follow-dep");

    delegation::delegate_task(
        project_dir,
        &lead,
        &worker,
        &task1,
        "续派验证：前置任务",
    )?;
    delegation::report_task_auto(
        project_dir,
        &worker,
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
    let worker = pick_non_lead(&agents, &lead);
    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    let task1 = unique_task("tx-follow-dep");

    delegation::delegate_task(
        project_dir,
        &lead,
        &worker,
        &task1,
        "transcript 续派验证",
    )?;
    delegation::report_task_auto(
        project_dir,
        &worker,
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

    let mut lead_watch = lead_watch::LeadWatchState::new(project_dir, &lead);
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

/// Relay must not skip events when target PTY is not ready.
pub fn verify_relay_cursor_hold() -> Result<()> {
    use std::time::Duration;

    use crate::meta::CoordEvent;
    use crate::relay::{dispatch, RelayConfig, RelayState};

    let events = vec![CoordEvent {
        line_no: 1,
        time: Some("2026-01-01T00:00:00Z".into()),
        kind: "notify".into(),
        agent: Some("codex".into()),
        message: "【委派·relay-hold】请开始".into(),
        from: "cursor".into(),
        task: None,
        action: None,
    }];
    let names = vec!["cursor".into(), "codex".into()];
    let mut panes: Vec<Option<crate::pane::AgentPane>> = vec![None, None];
    let mut state = RelayState::new(std::path::Path::new("."), 2);
    let config = RelayConfig {
        enabled: true,
        cooldown: Duration::from_secs(12),
    };
    let delivered = dispatch(&config, &mut state, &names, &mut panes, &events);
    if delivered != 0 {
        bail!("relay hold: expected 0 deliveries without PTY");
    }
    if state.cursor != 0 {
        bail!("relay hold: cursor advanced without delivery (cursor={})", state.cursor);
    }
    Ok(())
}

/// Plan fingerprints survive LeadWatchState restart (persistent dedupe).
pub fn verify_persistent_plan_dedupe(project_dir: &Path) -> Result<()> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let worker = pick_non_lead(&agents, &lead);
    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    let task = unique_task("dedupe-plan");
    let text = format!(
        r#"
```agent-plan
[{{"worker":"{worker}","task":"{task}","description":"dedupe"}}]
```
"#,
        worker = worker, task = task
    );
    let mut first = lead_watch::LeadWatchState::new(project_dir, &lead);
    let n1 = lead_watch::plans_from_text(
        &text,
        &names,
        first.seen_plans_mut(),
        Some(project_dir),
    );
    if n1.len() != 1 {
        bail!("persistent dedupe: expected 1 plan item, got {}", n1.len());
    }
    let mut second = lead_watch::LeadWatchState::new(project_dir, &lead);
    let n2 = lead_watch::plans_from_text(
        &text,
        &names,
        second.seen_plans_mut(),
        Some(project_dir),
    );
    if !n2.is_empty() {
        bail!("persistent dedupe: restart re-dispatched {} items", n2.len());
    }
    Ok(())
}

/// Cyclic depends_on must not delegate any worker.
pub fn verify_dag_cycle_rejected(project_dir: &Path) -> Result<()> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let worker_a = pick_non_lead(&agents, &lead);
    let worker_b = agents
        .iter()
        .find(|a| a.name != lead && a.name != worker_a)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "kimi".into());
    let task_a = unique_task("cycle-a");
    let task_b = unique_task("cycle-b");
    let items = vec![
        lead_watch::PlanItem {
            worker: worker_a,
            task: task_a.clone(),
            description: "环检测 A".into(),
            depends_on: vec![task_b.clone()],
        },
        lead_watch::PlanItem {
            worker: worker_b,
            task: task_b.clone(),
            description: "环检测 B".into(),
            depends_on: vec![task_a.clone()],
        },
    ];
    let dispatched = lead_watch::dispatch_plan_items(project_dir, &lead, items, "verify-cycle");
    if dispatched != 0 {
        bail!("DAG cycle: expected 0 dispatch, got {dispatched}");
    }
    let entries = mailbox::load_entries(project_dir, 80);
    for task in [&task_a, &task_b] {
        if entries.iter().any(|e| {
            e.kind == "delegate" && e.task.as_deref() == Some(task.as_str())
        }) {
            bail!("DAG cycle: task {task} should not be delegated");
        }
    }
    Ok(())
}

/// sync-lead artifacts exist and contain orchestrator identity markers.
pub fn verify_lead_identity_sync(project_dir: &Path) -> Result<()> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let spec = agents
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(&lead))
        .ok_or_else(|| anyhow::anyhow!("lead {lead} missing from agents.yaml"))?;

    lead_identity::sync_lead_context(project_dir, &lead, &spec.worktree)?;
    agent_memory::refresh_lead_identity(project_dir, &lead)?;

    let playbook = project_dir.join(".agents/LEAD.md");
    let rules = spec
        .worktree
        .join(".cursor/rules/agent-tui-orchestrator.mdc");
    let stub = spec.worktree.join("AGENTS-agent-tui.md");

    for (label, path) in [
        ("LEAD.md", &playbook),
        ("orchestrator.mdc", &rules),
        ("AGENTS-agent-tui.md", &stub),
    ] {
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("read {label} {}", path.display()))?;
        if !body.contains("Lead") && !body.contains("Orchestrator") {
            bail!("{label} missing Lead/Orchestrator identity");
        }
        if !body.contains("agent-plan") {
            bail!("{label} missing agent-plan guidance");
        }
    }

    let rules_body = std::fs::read_to_string(&rules)?;
    if !rules_body.contains("alwaysApply: true") {
        bail!("orchestrator.mdc missing alwaysApply");
    }
    if !rules_body.contains(&lead) {
        bail!("orchestrator.mdc missing lead name {lead}");
    }
    if !rules_body.contains("优势委派") {
        bail!("orchestrator.mdc missing strength-based delegation guide");
    }

    let strengths = project_dir.join(".agents/STRENGTHS.md");
    if !strengths.is_file() {
        bail!("STRENGTHS.md missing after sync-lead");
    }
    let strengths_body = std::fs::read_to_string(&strengths)?;
    if !strengths_body.contains("优势委派") {
        bail!("STRENGTHS.md missing 优势委派 section");
    }

    let worker = pick_non_lead(&agents, &lead);

    let hint = crate::agent_strengths::delegation_mismatch(
        &agents,
        &lead,
        &worker,
        "ui-dashboard",
        "React dashboard 组件与 Tailwind 样式",
        Some(project_dir),
    );
    if hint.is_none() {
        bail!("delegation mismatch: expected UI task on {worker} to suggest kimi");
    }

    let agents_md = spec.worktree.join("AGENTS.md");
    if agents_md.is_file() {
        let content = std::fs::read_to_string(&agents_md)?;
        if !content.contains("agent-tui:lead") {
            bail!("AGENTS.md missing agent-tui:lead pointer");
        }
    }

    let mem = agent_memory::memory_md_path(project_dir, &lead);
    if mem.is_file() {
        let mem_body = std::fs::read_to_string(&mem)?;
        if !mem_body.contains("Lead") && !mem_body.contains("Orchestrator") {
            bail!("lead MEMORY.md missing identity rules");
        }
    }

    Ok(())
}

/// Outcomes logged from worker reports; evolve refreshes STRENGTHS history section.
pub fn verify_evolution_records(project_dir: &Path) -> Result<()> {
    let outcomes = crate::delegation_stats::load_outcomes(project_dir, 50);
    if outcomes.is_empty() {
        bail!("evolution: no delegation_outcomes.jsonl records after verify loop");
    }
    let n = crate::delegation_stats::evolve_project(project_dir)?;
    if n == 0 {
        bail!("evolution: evolve_project returned 0");
    }
    let strengths = std::fs::read_to_string(project_dir.join(".agents/STRENGTHS.md"))?;
    if !strengths.contains("历史表现") {
        bail!("STRENGTHS.md missing 历史表现 section after evolve");
    }
    Ok(())
}

/// relay_cursor.json survives restart (events + mailbox + plan_inbox offsets).
pub fn verify_relay_cursor_persist(project_dir: &Path) -> Result<()> {
    let before = coord_dedupe::load_relay_cursor(project_dir);
    let probe = RelayCursorState {
        events_cursor: before.events_cursor.saturating_add(1000),
        mailbox_line: before.mailbox_line.saturating_add(1000),
        plan_inbox_line: before.plan_inbox_line.saturating_add(1000),
        initial_briefing_sent: before.initial_briefing_sent,
        initial_briefing_lead: before.initial_briefing_lead.clone(),
    };
    coord_dedupe::save_relay_cursor(project_dir, &probe)?;
    let loaded = coord_dedupe::load_relay_cursor(project_dir);
    if loaded.events_cursor != probe.events_cursor
        || loaded.mailbox_line != probe.mailbox_line
        || loaded.plan_inbox_line != probe.plan_inbox_line
    {
        bail!("relay cursor reload mismatch: {loaded:?} vs {probe:?}");
    }
    let relay = RelayState::new(project_dir, 2);
    if relay.cursor != probe.events_cursor || relay.mailbox_line != probe.mailbox_line {
        bail!(
            "RelayState expected cursor={} mailbox={}, got {} {}",
            probe.events_cursor,
            probe.mailbox_line,
            relay.cursor,
            relay.mailbox_line
        );
    }
    let lw = lead_watch::LeadWatchState::new(project_dir, "cursor");
    if lw.plan_inbox_line() != probe.plan_inbox_line {
        bail!(
            "LeadWatchState plan_inbox_line expected {}",
            probe.plan_inbox_line
        );
    }
    coord_dedupe::save_relay_cursor(project_dir, &before)?;
    Ok(())
}

/// blocked + max nudges → auto-delegate to advisor without human.
pub fn verify_blocked_escalation(project_dir: &Path) -> Result<()> {
    let agents = load_agents(project_dir)?;
    let lead = resolve_lead_agent(&agents);
    let advisor = agents
        .iter()
        .find(|a| a.role == "advisor" && !a.name.eq_ignore_ascii_case(&lead))
        .map(|a| a.name.clone())
        .ok_or_else(|| anyhow::anyhow!("verify blocked: need advisor in agents.yaml"))?;

    let worker = pick_non_lead(&agents, &lead);
    let task = unique_task("blocked-esc");
    delegation::delegate_task(project_dir, &lead, &worker, &task, "blocked 升级验证")?;
    delegation::report_task_auto(
        project_dir,
        &worker,
        &lead,
        &task,
        "blocked",
        "缺少外部 API 凭证",
    )?;

    let path = project_dir.join(".agents/shared/lead_followup.jsonl");
    let content = std::fs::read_to_string(&path)?;
    let max = std::env::var("AGENT_TUI_LEAD_FOLLOWUP_MAX_NUDGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let mut lines: Vec<String> = Vec::new();
    let mut found = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(mut rec) = serde_json::from_str::<lead_followup::FollowupRecord>(trimmed) else {
            lines.push(line.to_string());
            continue;
        };
        if rec.task == task {
            rec.nudge_count = max;
            rec.escalated = false;
            found = true;
        }
        lines.push(serde_json::to_string(&rec)?);
    }
    if !found {
        bail!("blocked escalation: missing followup record for {task}");
    }
    std::fs::write(&path, format!("{}\n", lines.join("\n")))?;

    let n = lead_followup::try_blocked_escalations(project_dir, &lead);
    if n != 1 {
        bail!("blocked escalation: expected 1 delegate, got {n}");
    }
    let unblock = format!("{task}-unblock");
    let entries = mailbox::load_entries(project_dir, 50);
    if !entries.iter().any(|e| {
        e.kind == "delegate" && e.to == advisor && e.task.as_deref() == Some(unblock.as_str())
    }) {
        bail!("blocked escalation: missing advisor delegate for {unblock}");
    }
    Ok(())
}

/// Implementation done → auto review → parent done → merge-ready notify.
pub fn verify_review_merge_chain() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("review-merge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    project_init::init_project(
        &dir,
        &InitOptions {
            force: true,
            link_worktrees: false,
            sync_lead: false,
            minimal: false,
        },
    )?;
    let agents = load_agents(&dir)?;
    let lead = resolve_lead_agent(&agents);
    if !agents.iter().any(|a| a.name == "mimo") {
        bail!("review merge verify: need mimo reviewer");
    }

    std::env::set_var("AGENT_TUI_MERGE_BATCH", "rgate");
    std::env::remove_var("AGENT_TUI_REVIEW_GATE");
    let _ = merge_ready::reset_notified(&dir);

    let task = unique_task("rgate");
    delegation::delegate_task(&dir, &lead, "codex", &task, "review gate 验证")?;
    delegation::report_task_auto(&dir, "codex", &lead, &task, "done", "实现完成")?;

    match task_state::load_snapshots(&dir)
        .get(&task)
        .map(|s| s.status.as_str())
    {
        Some("awaiting_review") => {}
        other => bail!("review gate: expected awaiting_review, got {other:?}"),
    }

    let review_id = review_gate::review_task_id(&task);
    match task_state::load_snapshots(&dir)
        .get(&review_id)
        .map(|s| s.status.as_str())
    {
        Some("delegated") => {}
        other => bail!("review gate: expected review delegated, got {other:?}"),
    }

    delegation::report_task_auto(&dir, "mimo", &lead, &review_id, "done", "LGTM")?;

    match task_state::load_snapshots(&dir)
        .get(&task)
        .map(|s| s.status.as_str())
    {
        Some("done") => {}
        other => bail!("review gate: parent expected done after review, got {other:?}"),
    }

    let batch_id = crate::batch_review::batch_review_task_id(std::slice::from_ref(&task));
    match task_state::load_snapshots(&dir)
        .get(&batch_id)
        .map(|s| s.status.as_str())
    {
        Some("delegated") => {}
        other => bail!("batch review: expected delegated, got {other:?}"),
    }

    let pre_events = meta::load_coord_events(&dir);
    if pre_events.iter().any(|e| e.message.contains("【merge-ready】")) {
        bail!("merge-ready should wait for batch review");
    }

    delegation::report_task_auto(&dir, "mimo", &lead, &batch_id, "done", "batch LGTM")?;
    let _ = merge_ready::notify_lead_if_ready(&dir, &lead)?;

    let events = meta::load_coord_events(&dir);
    if !events.iter().any(|e| e.message.contains("【merge-ready】")) {
        bail!("merge-ready: missing notify to lead");
    }

    let smoke_id = crate::post_merge_smoke::smoke_task_id(std::slice::from_ref(&task));
    match task_state::load_snapshots(&dir)
        .get(&smoke_id)
        .map(|s| s.status.as_str())
    {
        Some("delegated") => {}
        other => bail!("post-merge smoke: expected delegated after merge-ready, got {other:?}"),
    }

    delegation::report_task_auto(&dir, "mimo", &lead, &smoke_id, "done", "smoke ok")?;

    let wf = crate::workflow_phase::evaluate(&dir);
    if wf.phase != crate::workflow_phase::Phase::MergeReady {
        bail!(
            "workflow after smoke: expected MergeReady, got {:?}",
            wf.phase
        );
    }

    std::env::remove_var("AGENT_TUI_MERGE_BATCH");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// smoke 通过后 auto-pr 路径（无 gh 时跳过创建，仍验证门禁不 panic）。
pub fn verify_smoke_auto_pr_chain() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("smoke-apr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    project_init::init_project(
        &dir,
        &InitOptions {
            force: true,
            link_worktrees: false,
            sync_lead: false,
            minimal: false,
        },
    )?;
    let agents = load_agents(&dir)?;
    let lead = resolve_lead_agent(&agents);
    if !agents.iter().any(|a| a.name == "mimo") {
        bail!("smoke auto-pr verify: need mimo reviewer");
    }

    std::env::set_var("AGENT_TUI_MERGE_BATCH", "smoke");
    std::env::set_var("AGENT_TUI_AUTO_PR", "1");
    std::env::remove_var("AGENT_TUI_REVIEW_GATE");
    let _ = merge_ready::reset_notified(&dir);

    let task = unique_task("smoke");
    delegation::delegate_task(&dir, &lead, "codex", &task, "smoke 链验证")?;
    delegation::report_task_auto(&dir, "codex", &lead, &task, "done", "ok")?;
    let review_id = review_gate::review_task_id(&task);
    delegation::report_task_auto(&dir, "mimo", &lead, &review_id, "done", "ok")?;
    let batch_id = crate::batch_review::batch_review_task_id(std::slice::from_ref(&task));
    delegation::report_task_auto(&dir, "mimo", &lead, &batch_id, "done", "ok")?;
    let _ = merge_ready::notify_lead_if_ready(&dir, &lead)?;

    let smoke_id = crate::post_merge_smoke::smoke_task_id(std::slice::from_ref(&task));
    if !task_state::load_snapshots(&dir).contains_key(&smoke_id) {
        bail!("smoke auto-pr: smoke task not dispatched");
    }
    delegation::report_task_auto(&dir, "mimo", &lead, &smoke_id, "done", "tests pass")?;

    // auto_pr may skip without gh/feature branch — must not error
    let _ = crate::auto_pr::maybe_create(&dir, std::slice::from_ref(&task))?;

    let obs = observer::build_snapshot(&dir)?;
    if obs.workflow.title.is_empty() {
        bail!("observer workflow empty after smoke chain");
    }

    std::env::remove_var("AGENT_TUI_MERGE_BATCH");
    std::env::remove_var("AGENT_TUI_AUTO_PR");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// simulate PR merged → poll → post-github-merge → shipped.
pub fn verify_pr_merged_chain() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("pr-merged-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    project_init::init_project(
        &dir,
        &InitOptions {
            force: true,
            link_worktrees: false,
            sync_lead: false,
            minimal: false,
        },
    )?;
    let agents = load_agents(&dir)?;
    let lead = resolve_lead_agent(&agents);

    std::env::set_var("AGENT_TUI_MERGE_BATCH", "pmerge");
    std::env::remove_var("AGENT_TUI_REVIEW_GATE");
    let _ = merge_ready::reset_notified(&dir);

    let task = unique_task("pmerge");
    delegation::delegate_task(&dir, &lead, "codex", &task, "pr merged 链")?;
    delegation::report_task_auto(&dir, "codex", &lead, &task, "done", "ok")?;
    let review_id = review_gate::review_task_id(&task);
    delegation::report_task_auto(&dir, "mimo", &lead, &review_id, "done", "ok")?;
    let batch_id = crate::batch_review::batch_review_task_id(std::slice::from_ref(&task));
    delegation::report_task_auto(&dir, "mimo", &lead, &batch_id, "done", "ok")?;
    let _ = merge_ready::notify_lead_if_ready(&dir, &lead)?;

    let smoke_id = crate::post_merge_smoke::smoke_task_id(std::slice::from_ref(&task));
    delegation::report_task_auto(&dir, "mimo", &lead, &smoke_id, "done", "ok")?;

    let fp = merge_ready::done_tasks_fingerprint(std::slice::from_ref(&task));
    let ap_path = dir.join(".agents/shared/auto_pr_dispatched.jsonl");
    std::fs::write(&ap_path, format!("{fp}\n"))?;

    crate::pr_lifecycle::simulate_merged(&dir)?;
    std::env::set_var("AGENT_TUI_POLL_PR", "1");
    let polled = crate::pr_lifecycle::maybe_poll_merged(&dir, &lead)?;
    if !polled {
        bail!("pr merged: maybe_poll_merged expected true");
    }

    let gh_id = crate::pr_lifecycle::post_github_merge_task_id(std::slice::from_ref(&task));
    match task_state::load_snapshots(&dir)
        .get(&gh_id)
        .map(|s| s.status.as_str())
    {
        Some("delegated") => {}
        other => bail!("post-github-merge: expected delegated, got {other:?}"),
    }

    let events = meta::load_coord_events(&dir);
    if !events.iter().any(|e| e.message.contains("【pr-merged】")) {
        bail!("pr merged: missing notify");
    }

    delegation::report_task_auto(&dir, "mimo", &lead, &gh_id, "done", "main ok")?;
    let wf = crate::workflow_phase::evaluate(&dir);
    if wf.phase != crate::workflow_phase::Phase::Shipped {
        bail!("workflow after post-github-merge: expected Shipped, got {:?}", wf.phase);
    }

    std::env::remove_var("AGENT_TUI_MERGE_BATCH");
    std::env::remove_var("AGENT_TUI_POLL_PR");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// CI pass simulate → auto-merge → poll → post-github-merge → shipped.
pub fn verify_auto_merge_chain() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("auto-merge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    project_init::init_project(
        &dir,
        &InitOptions {
            force: true,
            link_worktrees: false,
            sync_lead: false,
            minimal: false,
        },
    )?;
    let agents = load_agents(&dir)?;
    let lead = resolve_lead_agent(&agents);

    std::env::set_var("AGENT_TUI_MERGE_BATCH", "amerge");
    std::env::remove_var("AGENT_TUI_REVIEW_GATE");
    let _ = merge_ready::reset_notified(&dir);

    let task = unique_task("amerge");
    delegation::delegate_task(&dir, &lead, "codex", &task, "auto merge 链")?;
    delegation::report_task_auto(&dir, "codex", &lead, &task, "done", "ok")?;
    let review_id = review_gate::review_task_id(&task);
    delegation::report_task_auto(&dir, "mimo", &lead, &review_id, "done", "ok")?;
    let batch_id = crate::batch_review::batch_review_task_id(std::slice::from_ref(&task));
    delegation::report_task_auto(&dir, "mimo", &lead, &batch_id, "done", "ok")?;
    let _ = merge_ready::notify_lead_if_ready(&dir, &lead)?;

    let smoke_id = crate::post_merge_smoke::smoke_task_id(std::slice::from_ref(&task));
    delegation::report_task_auto(&dir, "mimo", &lead, &smoke_id, "done", "ok")?;

    let fp = merge_ready::done_tasks_fingerprint(std::slice::from_ref(&task));
    std::fs::write(
        dir.join(".agents/shared/auto_pr_dispatched.jsonl"),
        format!("{fp}\n"),
    )?;

    crate::pr_lifecycle::simulate_ci_pass(&dir)?;
    std::env::set_var("AGENT_TUI_AUTO_MERGE", "1");
    std::env::set_var("AGENT_TUI_POLL_PR", "1");

    let merged = crate::pr_lifecycle::maybe_auto_merge(&dir, &lead)?;
    if !merged {
        bail!("auto-merge: maybe_auto_merge expected true");
    }
    if !crate::pr_lifecycle::was_auto_merge_dispatched(&dir, std::slice::from_ref(&task)) {
        bail!("auto-merge: missing dispatched fingerprint");
    }

    let events = meta::load_coord_events(&dir);
    if !events.iter().any(|e| e.message.contains("【auto-merge】")) {
        bail!("auto-merge: missing notify");
    }

    let polled = crate::pr_lifecycle::maybe_poll_merged(&dir, &lead)?;
    if !polled {
        bail!("auto-merge: maybe_poll_merged expected true after simulate merge");
    }

    let gh_id = crate::pr_lifecycle::post_github_merge_task_id(std::slice::from_ref(&task));
    delegation::report_task_auto(&dir, "mimo", &lead, &gh_id, "done", "main ok")?;
    let wf = crate::workflow_phase::evaluate(&dir);
    if wf.phase != crate::workflow_phase::Phase::Shipped {
        bail!(
            "auto-merge workflow: expected Shipped, got {:?}",
            wf.phase
        );
    }

    std::env::remove_var("AGENT_TUI_MERGE_BATCH");
    std::env::remove_var("AGENT_TUI_AUTO_MERGE");
    std::env::remove_var("AGENT_TUI_POLL_PR");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

pub fn run_all(project_dir: &Path) -> Result<()> {
    let _pre = crate::verify_cleanup::cleanup_verify_artifacts(project_dir)
        .context("pre-cleanup verify artifacts")?;
    verify_parsers().context("parser checks")?;
    verify_relay_cursor_hold().context("relay cursor hold")?;
    verify_persistent_plan_dedupe(project_dir).context("persistent plan dedupe")?;
    let outcome = without_review_gate(|| verify_events_chain(project_dir)).context("events chain")?;
    let retry_outcome =
        without_review_gate(|| verify_failed_retry_chain(project_dir)).context("failed→retry chain")?;
    let dag_outcome = without_review_gate(|| verify_dag_chain(project_dir)).context("DAG chain")?;
    without_review_gate(|| verify_lead_followup_chain(project_dir)).context("lead followup chain")?;
    without_review_gate(|| verify_lead_transcript_followup_chain(project_dir))
        .context("lead transcript followup")?;
    without_review_gate(|| verify_dag_cycle_rejected(project_dir)).context("DAG cycle rejection")?;
    verify_lead_identity_sync(project_dir).context("lead identity sync")?;
    verify_relay_cursor_persist(project_dir).context("relay cursor persist")?;
    verify_blocked_escalation(project_dir).context("blocked advisor escalate")?;
    verify_review_merge_chain().context("review gate + merge-ready + smoke")?;
    verify_smoke_auto_pr_chain().context("smoke + auto-pr + observer workflow")?;
    verify_pr_merged_chain().context("pr merged poll + post-github-merge")?;
    verify_auto_merge_chain().context("auto-merge ci gate + shipped")?;
    verify_evolution_records(project_dir).context("delegation evolution")?;
    crate::project_init::verify_init_scaffold().context("init scaffold")?;
    observer::verify_http_snapshot(project_dir).context("observer HTTP")?;
    observer::verify_sse_stream(project_dir).context("observer SSE")?;
    let snap = observer::build_snapshot(project_dir).context("observer snapshot")?;
    if !snap.tasks.iter().any(|t| t.task == outcome.task && t.status == "done") {
        anyhow::bail!("observer snapshot missing done task {}", outcome.task);
    }

    let cleanup = crate::verify_cleanup::cleanup_verify_artifacts(project_dir)
        .context("cleanup verify artifacts")?;
    if cleanup.tasks_removed > 0 {
        println!(
            "  cleanup: 已移除 {} 条 verify-loop 残留",
            cleanup.tasks_removed
        );
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
    println!("  DAG cycle: 环依赖拒绝委派 ✓");
    println!("  lead identity: sync-lead + orchestrator.mdc + LEAD.md + STRENGTHS ✓");
    println!("  strength delegate: UI→codex 错配提示 ✓");
    println!("  relay persist: relay_cursor.json 重启恢复 ✓");
    println!("  blocked: max nudges → advisor 自动升级 ✓");
    println!("  review gate: done → task-review → batch-review → merge-ready → smoke ✓");
    println!("  smoke/auto-pr: smoke 门禁 → workflow/observer 阶段 ✓");
    println!("  pr-merged: poll → post-github-merge → shipped ✓");
    println!("  auto-merge: CI/review 门禁 → merge → shipped ✓");
    println!("  evolution: outcomes → STRENGTHS 历史表现 ✓");
    println!("  init scaffold: 任意目录 agent-tui init ✓");
    println!("  relay: PTY 未就绪时不推进游标 ✓");
    println!("  dedupe: plan 指纹重启后仍有效 ✓");
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
