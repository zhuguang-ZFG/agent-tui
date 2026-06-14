use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use regex::Regex;
use serde::Deserialize;

use crate::agent_memory;
use crate::config::normalize_windows_path;
use crate::delegation;
use crate::mailbox;
use crate::meta;
use crate::pane::AgentPane;
use crate::task_dag;
use crate::task_state;
use crate::terminal;

pub const BRIEFING_DELAY: Duration = Duration::from_secs(6);
/// Delay before re-injecting rules after lead pane restart (CLI warm-up).
pub const BRIEFING_RESTART_DELAY: Duration = Duration::from_secs(6);
const BRIEFING_RETRY_DELAYS: &[Duration] = &[
    Duration::from_secs(3),
    Duration::from_secs(8),
    Duration::from_secs(15),
];
const DEFAULT_BRIEFING_INTERVAL_SECS: u64 = 7200;
const DEFAULT_BRIEFING_EVERY_REPORTS: u32 = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct PlanItem {
    pub worker: String,
    pub task: String,
    #[serde(default)]
    pub description: String,
    /// Task ids that must be `done` before this item is dispatched.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

pub struct LeadWatchState {
    seen_plans: HashSet<String>,
    briefing_sent: bool,
    briefing_after: Option<Instant>,
    /// PTY re-inject retries (cursor CLI may still be loading at first deadline).
    briefing_retries: Vec<Instant>,
    last_briefing_at: Option<Instant>,
    reports_since_briefing: u32,
    /// Deferred refresh after lead pane restart.
    rebrief_after: Option<Instant>,
    /// Lines already consumed from plan_inbox.jsonl.
    plan_inbox_line: usize,
    /// Dedupe for agent-plan blocks parsed during followup tail scan (separate from seen_plans).
    followup_plan_seen: HashSet<String>,
}

impl LeadWatchState {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            seen_plans: crate::coord_dedupe::load_plan_fingerprints(project_dir),
            briefing_sent: false,
            briefing_after: None,
            briefing_retries: Vec::new(),
            last_briefing_at: None,
            reports_since_briefing: 0,
            rebrief_after: None,
            plan_inbox_line: 0,
            followup_plan_seen: HashSet::new(),
        }
    }

    pub fn clear_followup_scan(&mut self) {
        self.followup_plan_seen.clear();
    }

    pub fn followup_plan_seen_mut(&mut self) -> &mut HashSet<String> {
        &mut self.followup_plan_seen
    }

    pub fn seen_plans_mut(&mut self) -> &mut HashSet<String> {
        &mut self.seen_plans
    }

    pub fn arm_briefing(&mut self, after: Instant) {
        self.briefing_after = Some(after);
        self.briefing_sent = false;
        self.briefing_retries.clear();
        self.rebrief_after = None;
    }
}

fn periodic_rebrief_enabled() -> bool {
    std::env::var("AGENT_TUI_PERIODIC_BRIEFING")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn briefing_interval() -> Option<Duration> {
    if !periodic_rebrief_enabled() {
        return None;
    }
    let secs = std::env::var("AGENT_TUI_BRIEFING_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_BRIEFING_INTERVAL_SECS);
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    }
}

fn briefing_every_reports() -> Option<u32> {
    if !periodic_rebrief_enabled() {
        return None;
    }
    let n = std::env::var("AGENT_TUI_BRIEFING_EVERY_REPORTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_BRIEFING_EVERY_REPORTS);
    if n == 0 {
        None
    } else {
        Some(n)
    }
}

fn auto_dispatch_enabled() -> bool {
    std::env::var("AGENT_TUI_AUTO_DISPATCH")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// User submits a high-level task to the lead agent; lead decomposes and emits `agent-plan`.
pub fn submit_user_task(project_dir: &Path, lead: &str, message: &str) -> Result<()> {
    let message = message.trim();
    if message.is_empty() {
        anyhow::bail!("任务描述不能为空");
    }
    let body = format!(
        "【用户任务】{message}\n\n\
         请分析需求、拆成可并行子任务，输出 agent-plan 代码块（JSON 数组），TUI 自动派发。\
         工人完成后会 agent-report 自动回传；你收到回执后请自动输出下一波 agent-plan（review/修复/续派），无需等用户。"
    );
    meta::notify_agent_from(project_dir, lead, &body, "user")?;
    meta::append_shared_line(project_dir, &format!("用户 → {lead} 任务：{message}"))?;
    let _ = mailbox::user_task(project_dir, lead, message);
    let _ = agent_memory::on_user_task(project_dir, lead, &body);
    Ok(())
}

fn extract_agent_plan_blocks(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?s)```\s*agent-plan\s*\r?\n?(.*?)\r?\n?```").expect("agent-plan regex")
    });
    re.captures_iter(text)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Fallback when fences are stripped by the terminal renderer.
fn extract_loose_plan_arrays(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?s)\[\s*\{[^\]]*"worker"\s*:[^\]]*\}\s*\]"#).expect("loose plan regex")
    });
    re.find_iter(text)
        .map(|m| m.as_str().trim().to_string())
        .collect()
}

pub fn parse_plan_items(json: &str, agent_names: &[String]) -> Vec<PlanItem> {
    let Ok(items) = serde_json::from_str::<Vec<PlanItem>>(json) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter(|item| {
            agent_names
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&item.worker))
        })
        .collect()
}

pub fn collect_plan_blocks(text: &str) -> Vec<String> {
    let mut blocks = extract_agent_plan_blocks(text);
    for loose in extract_loose_plan_arrays(text) {
        if !blocks.iter().any(|b| b == &loose) {
            blocks.push(loose);
        }
    }
    blocks
}

fn plan_fingerprint(block: &str, items: &[PlanItem]) -> String {
    if !items.is_empty() {
        let mut parts: Vec<String> = items
            .iter()
            .map(|i| format!("{}:{}", i.worker, i.task))
            .collect();
        parts.sort();
        return parts.join("|");
    }
    block.trim().to_string()
}

/// Parse transcript text and return delegatable plan items (deduped by fingerprint).
pub fn plans_from_text(
    text: &str,
    agent_names: &[String],
    seen: &mut HashSet<String>,
    project_dir: Option<&Path>,
) -> Vec<PlanItem> {
    let mut out = Vec::new();
    for block in collect_plan_blocks(text) {
        let items = parse_plan_items(&block, agent_names);
        if items.is_empty() {
            continue;
        }
        let fp = plan_fingerprint(&block, &items);
        if seen.contains(&fp) {
            continue;
        }
        let mut block_ok = true;
        for item in &items {
            if meta::validate_task_name(&item.task).is_err() {
                block_ok = false;
                break;
            }
        }
        if !block_ok {
            continue;
        }
        seen.insert(fp.clone());
        if let Some(dir) = project_dir {
            crate::coord_dedupe::remember_plan_fingerprint(dir, &fp);
        }
        out.extend(items);
    }
    out
}

fn followup_satisfying_sources(source: &str) -> bool {
    !matches!(source, "cli" | "plan_dry_run")
}

/// Schedule and dispatch plan items (transcript, plan_inbox, or CLI).
pub fn dispatch_plan_items(
    project_dir: &Path,
    lead: &str,
    items: Vec<PlanItem>,
    source: &str,
) -> usize {
    if items.is_empty() {
        return 0;
    }
    let had_pending = crate::lead_followup::pending_count(project_dir) > 0;
    if source != "plan_inbox" {
        let _ = crate::plan_inbox::append_items(project_dir, lead, &items, source);
    }
    for item in &items {
        let _ = mailbox::plan_dispatch(
            project_dir,
            lead,
            &item.worker,
            &item.task,
            &item.description,
            source,
        );
    }

    let completed = task_dag::load_completed_tasks(project_dir);
    let (ready, still_pending) = match task_dag::schedule_plans(project_dir, items, &completed) {
        Ok(pair) => pair,
        Err(e) => {
            terminal::log_message(
                project_dir,
                "warn",
                &format!("task DAG schedule failed: {e:#}"),
            );
            return 0;
        }
    };
    for item in &still_pending {
        let _ = task_state::on_plan_pending(project_dir, lead, &item.worker, &item.task);
    }
    if !still_pending.is_empty() {
        terminal::log_message(
            project_dir,
            "info",
            &format!(
                "task DAG: {} 个子任务等待依赖（pending_plans.jsonl）",
                still_pending.len()
            ),
        );
    }

    let mut executed = 0usize;
    for item in ready {
        if item.worker.eq_ignore_ascii_case(lead) {
            continue;
        }
        match delegation::delegate_task(
            project_dir,
            lead,
            &item.worker,
            &item.task,
            &item.description,
        ) {
            Ok(()) => executed += 1,
            Err(e) => {
                terminal::log_message(
                    project_dir,
                    "warn",
                    &format!(
                        "auto-plan {}/{} failed: {e:#}",
                        item.worker, item.task
                    ),
                );
            }
        }
    }
    if executed > 0 {
        let summary = format!("主 Agent 按计划派发 {executed} 个子任务（{source}）");
        let _ = agent_memory::on_plan_dispatch(project_dir, lead, &summary);
        terminal::log_message(project_dir, "info", &summary);
        if had_pending && followup_satisfying_sources(source) {
            let _ = crate::lead_followup::mark_all_satisfied(
                project_dir,
                &format!("plan_dispatch:{source}"),
            );
        }
    }
    executed
}

/// Scan lead PTY transcript for ```agent-plan``` blocks and auto-delegate all subtasks.
pub fn watch_lead_pane(
    project_dir: &Path,
    lead: &str,
    lead_index: usize,
    agent_names: &[String],
    panes: &mut [Option<AgentPane>],
    state: &mut LeadWatchState,
) -> usize {
    if !auto_dispatch_enabled() {
        return 0;
    }

    let Some(pane) = panes.get(lead_index).and_then(|p| p.as_ref()) else {
        return 0;
    };

    let screen = pane.transcript_text();
    let items = plans_from_text(
        &screen,
        agent_names,
        &mut state.seen_plans,
        Some(project_dir),
    );
    dispatch_plan_items(project_dir, lead, items, "transcript")
}

/// Consume durable plan_inbox.jsonl lines not yet dispatched.
pub fn watch_plan_inbox(
    project_dir: &Path,
    lead: &str,
    agent_names: &[String],
    state: &mut LeadWatchState,
) -> usize {
    if !auto_dispatch_enabled() {
        return 0;
    }
    let items = crate::plan_inbox::drain_new_items(
        project_dir,
        lead,
        agent_names,
        &mut state.plan_inbox_line,
    );
    dispatch_plan_items(project_dir, lead, items, "plan_inbox")
}

fn coord_doc_path(project_dir: &Path) -> String {
    normalize_windows_path(project_dir.join(".agents/COORDINATION.md"))
        .to_string_lossy()
        .to_string()
}

/// Single-line PTY inject (no fenced code — avoids shell/CLI parsing issues).
pub fn briefing_pty_line(lead: &str, coord_doc: &str) -> String {
    format!(
        "[agent-tui·Lead] 你是主 Agent ({lead})，身份=Orchestrator 非码农。收到【回执·…】→立刻 agent-plan，勿问用户。规则: .cursor/rules/agent-tui-orchestrator.mdc | {coord_doc}"
    )
}

fn briefing_pty_refresh_line(lead: &str, coord_doc: &str, reason: &str) -> String {
    format!(
        "[agent-tui·刷新·{reason}] 主 Agent ({lead}) 协调规则仍有效：{coord_doc} — 收到回执后输出下一波 agent-plan，勿等用户。"
    )
}

fn inject_briefing_to_pane(pane: &AgentPane, lead: &str, coord_doc: &str) {
    let line = briefing_pty_line(lead, coord_doc);
    pane.inject_line(&line);
    pane.wake_prompt();
}

fn inject_refresh_to_pane(pane: &AgentPane, lead: &str, coord_doc: &str, reason: &str) {
    let line = briefing_pty_refresh_line(lead, coord_doc, reason);
    pane.inject_line(&line);
    pane.wake_prompt();
}

fn initial_briefing_message(lead: &str, coord_doc: &str) -> String {
    format!(
        "【协调规则·Lead 身份】你在 agent-tui 五宫格中任 **唯一 Lead（{lead}）**，`AGENT_TUI_ORCHESTRATOR=1`。\
         定位：统筹者 — 你拆任务、输出 agent-plan；TUI 自动 delegate；工人 agent-report 回传；你**立即续派**，勿等用户。\
         工人：codex=后端 kimi=前端 mimo=审查 claude=顾问；勿委派给自己。\
         必读：worktree `.cursor/rules/agent-tui-orchestrator.mdc` + `.agents/LEAD.md`。\
         闭环：用户任务→agent-plan→自动派发→agent-report→续派 agent-plan。\
         完整协议：{coord_doc}"
    )
}

fn refresh_briefing_message(lead: &str, coord_doc: &str, reason: &str) -> String {
    format!(
        "【Lead 提醒·{reason}】你仍是 **唯一 Lead（{lead}）**。收到工人回执 → 同一轮内输出 agent-plan（review/续派/修复），禁止问用户是否继续。\
         规则：agent-tui-orchestrator.mdc + LEAD.md。协议：{coord_doc}"
    )
}

fn deliver_initial_briefing(
    project_dir: &Path,
    lead: &str,
    state: &mut LeadWatchState,
    lead_pane: Option<&AgentPane>,
) -> Result<()> {
    let coord_doc = coord_doc_path(project_dir);
    meta::notify_agent_from(
        project_dir,
        lead,
        &initial_briefing_message(lead, &coord_doc),
        "system",
    )?;
    state.briefing_sent = true;
    state.last_briefing_at = Some(Instant::now());
    state.reports_since_briefing = 0;
    arm_briefing_retries(state);

    if let Some(pane) = lead_pane {
        inject_briefing_to_pane(pane, lead, &coord_doc);
        terminal::log_message(
            project_dir,
            "info",
            &format!("briefing injected into lead PTY ({lead})"),
        );
    } else {
        terminal::log_message(
            project_dir,
            "warn",
            &format!("briefing notify written but lead pane not ready ({lead})"),
        );
    }
    Ok(())
}

fn deliver_refresh_briefing(
    project_dir: &Path,
    lead: &str,
    state: &mut LeadWatchState,
    lead_pane: Option<&AgentPane>,
    reason: &str,
) -> Result<bool> {
    if !state.briefing_sent {
        return Ok(false);
    }
    let coord_doc = coord_doc_path(project_dir);
    meta::notify_agent_from(
        project_dir,
        lead,
        &refresh_briefing_message(lead, &coord_doc, reason),
        "system",
    )?;
    state.last_briefing_at = Some(Instant::now());
    state.reports_since_briefing = 0;

    if let Some(pane) = lead_pane {
        inject_refresh_to_pane(pane, lead, &coord_doc, reason);
    }
    terminal::log_message(
        project_dir,
        "info",
        &format!("briefing refresh ({reason}) for lead ({lead})"),
    );
    Ok(true)
}

/// Lead pane restarted — schedule rules refresh after CLI warm-up.
pub fn on_lead_pane_restarted(state: &mut LeadWatchState) {
    if state.briefing_sent {
        state.rebrief_after = Some(Instant::now() + BRIEFING_RESTART_DELAY);
    } else {
        state.arm_briefing(Instant::now() + BRIEFING_RESTART_DELAY);
    }
}

/// Fire deferred refresh after lead pane restart.
pub fn process_pending_rebrief(
    project_dir: &Path,
    lead: &str,
    state: &mut LeadWatchState,
    lead_pane: Option<&AgentPane>,
) -> Result<bool> {
    let Some(deadline) = state.rebrief_after else {
        return Ok(false);
    };
    if Instant::now() < deadline {
        return Ok(false);
    }
    state.rebrief_after = None;
    deliver_refresh_briefing(project_dir, lead, state, lead_pane, "pane-restart")
}

/// Time-based refresh so long sessions do not drift from orchestrator role.
pub fn maybe_periodic_rebrief(
    project_dir: &Path,
    lead: &str,
    state: &mut LeadWatchState,
    lead_pane: Option<&AgentPane>,
) -> Result<bool> {
    let Some(interval) = briefing_interval() else {
        return Ok(false);
    };
    let Some(last) = state.last_briefing_at else {
        return Ok(false);
    };
    if last.elapsed() < interval {
        return Ok(false);
    }
    deliver_refresh_briefing(project_dir, lead, state, lead_pane, "periodic")
}

/// Refresh after N worker reports (default 5).
pub fn maybe_rebrief_after_reports(
    project_dir: &Path,
    lead: &str,
    state: &mut LeadWatchState,
    lead_pane: Option<&AgentPane>,
    new_reports: usize,
) -> Result<bool> {
    if new_reports == 0 {
        return Ok(false);
    }
    state.reports_since_briefing = state
        .reports_since_briefing
        .saturating_add(new_reports as u32);
    let Some(every) = briefing_every_reports() else {
        return Ok(false);
    };
    if state.reports_since_briefing < every {
        return Ok(false);
    }
    state.reports_since_briefing = 0;
    deliver_refresh_briefing(project_dir, lead, state, lead_pane, "reports")
}

fn arm_briefing_retries(state: &mut LeadWatchState) {
    let base = Instant::now();
    state.briefing_retries = BRIEFING_RETRY_DELAYS
        .iter()
        .map(|d| base + *d)
        .collect();
}

/// Fire scheduled PTY re-injects until cursor CLI is ready to accept input.
pub fn process_briefing_retries(
    project_dir: &Path,
    lead: &str,
    state: &mut LeadWatchState,
    lead_pane: Option<&AgentPane>,
) -> usize {
    if !state.briefing_sent {
        return 0;
    }
    let now = Instant::now();
    let mut fired = 0usize;
    state.briefing_retries.retain(|at| {
        if *at > now {
            return true;
        }
        if let Some(pane) = lead_pane {
            inject_briefing_to_pane(pane, lead, &coord_doc_path(project_dir));
            fired += 1;
        }
        false
    });
    fired
}

/// Notify lead + inject coordination prompt into cursor PTY (inbox record + direct inject).
pub fn maybe_send_briefing(
    project_dir: &Path,
    lead: &str,
    state: &mut LeadWatchState,
    lead_pane: Option<&AgentPane>,
) -> Result<bool> {
    if state.briefing_sent {
        return Ok(false);
    }
    let Some(deadline) = state.briefing_after else {
        return Ok(false);
    };
    if Instant::now() < deadline {
        return Ok(false);
    }

    deliver_initial_briefing(project_dir, lead, state, lead_pane)?;
    let _ = crate::agent_memory::refresh_lead_identity(project_dir, lead);
    let _ = agent_memory::record_event(
        project_dir,
        lead,
        &agent_memory::role_for(project_dir, lead),
        lead,
        agent_memory::MemoryEventKind::Briefing,
        "协调规则已注入（启动 briefing）",
        None,
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_agent_plan_block() {
        let text = r#"
一些说明
```agent-plan
[{"worker":"codex","task":"auth-api","description":"login"}]
```
"#;
        let blocks = collect_plan_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("codex"));
    }

    #[test]
    fn dedupes_same_plan() {
        let text = r#"
```agent-plan
[{"worker":"codex","task":"auth-api","description":"a"}]
```
```agent-plan
[{"worker":"codex","task":"auth-api","description":"b"}]
```
"#;
        let names = vec!["codex".into(), "cursor".into()];
        let mut seen = HashSet::new();
        let items = plans_from_text(text, &names, &mut seen, None);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn loose_array_fallback() {
        let text = r#"plan: [{"worker":"kimi","task":"ui-login","description":"页面"}] done"#;
        let names = vec!["kimi".into(), "cursor".into()];
        let mut seen = HashSet::new();
        let items = plans_from_text(text, &names, &mut seen, None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].task, "ui-login");
    }

    #[test]
    fn refresh_line_mentions_reason() {
        let line = briefing_pty_refresh_line("cursor", r"D:\p\COORD.md", "periodic");
        assert!(line.contains("刷新"));
        assert!(line.contains("periodic"));
    }
}
