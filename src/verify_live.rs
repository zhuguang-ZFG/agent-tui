//! Live PTY verification: inject coordination text and scan transcript parsers.

use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::lead_watch;
use crate::pane::spawn_demo_shell;
use crate::report_watch;

const POLL_MS: u64 = 100;
const TIMEOUT_MS: u64 = 4000;

pub fn run_live(project_dir: &Path) -> Result<()> {
    println!("Live 验证：PTY 注入 + transcript 解析…");

    verify_inject_roundtrip(project_dir).context("inject roundtrip")?;
    verify_plan_parser_live(project_dir).context("agent-plan live")?;
    verify_report_parser_live(project_dir).context("agent-report live")?;

    println!("Live 验证通过 ✓");
    println!("  PTY inject → transcript 可见");
    println!("  agent-plan / agent-report 解析器可读注入内容");
    Ok(())
}

fn verify_inject_roundtrip(project_dir: &Path) -> Result<()> {
    let lead = load_lead(project_dir)?;
    let pane = spawn_demo_shell("live-probe", 8, 80, project_dir, &lead)
        .context("spawn demo shell")?;
    let token = format!("agent-tui-live-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0));
    pane.inject_line(&token);
    wait_for_transcript(&pane, &token, TIMEOUT_MS)?;
    Ok(())
}

fn verify_plan_parser_live(project_dir: &Path) -> Result<()> {
    let lead = load_lead(project_dir)?;
    let agents = crate::config::load_agents(project_dir)?;
    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    let pane = spawn_demo_shell("live-plan", 12, 100, project_dir, &lead)?;
    let task = crate::verify_loop::unique_task("live-plan");
    let line = format!(
        "```agent-plan\n[{{\"worker\":\"codex\",\"task\":\"{task}\",\"description\":\"live\"}}]\n```"
    );
    pane.inject_line(&line);
    wait_for_transcript(&pane, &task, TIMEOUT_MS)?;
    let text = pane.transcript_text();
    let mut seen = std::collections::HashSet::new();
    let plans = lead_watch::plans_from_text(&text, &names, &mut seen, None);
    if plans.iter().any(|p| p.task == task) {
        return Ok(());
    }
    bail!("agent-plan not parsed from live transcript for task {task}");
}

fn verify_report_parser_live(project_dir: &Path) -> Result<()> {
    let lead = load_lead(project_dir)?;
    let pane = spawn_demo_shell("live-report", 12, 100, project_dir, &lead)?;
    let task = crate::verify_loop::unique_task("live-report");
    let line = format!(
        "```agent-report\n{{\"task\":\"{task}\",\"status\":\"done\",\"summary\":\"live ok\"}}\n```"
    );
    pane.inject_line(&line);
    wait_for_transcript(&pane, &task, TIMEOUT_MS)?;
    let text = pane.transcript_text();
    let mut seen = std::collections::HashSet::new();
    let reports = report_watch::reports_from_text("codex", &text, &mut seen, None);
    if reports.iter().any(|r| r.task == task && r.status == "done") {
        return Ok(());
    }
    bail!("agent-report not parsed from live transcript for task {task}");
}

fn load_lead(project_dir: &Path) -> Result<String> {
    let agents = crate::config::load_agents(project_dir)?;
    Ok(crate::config::resolve_lead_agent(&agents))
}

fn wait_for_transcript(
    pane: &crate::pane::AgentPane,
    needle: &str,
    timeout_ms: u64,
) -> Result<()> {
    let deadline = Duration::from_millis(timeout_ms);
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if pane.transcript_text().contains(needle) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(POLL_MS));
    }
    bail!("transcript timeout waiting for {needle:?}");
}
