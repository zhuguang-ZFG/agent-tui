//! MiMo Code–inspired per-agent memory (stage A): MEMORY.md + checkpoint.md + notes.md.
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::claims;
use crate::config;
use crate::meta::inbox_timestamp_iso;

pub fn role_for(project_dir: &Path, name: &str) -> String {
    config::load_agents(project_dir)
        .ok()
        .and_then(|agents| {
            agents
                .iter()
                .find(|a| a.name.eq_ignore_ascii_case(name))
                .map(|a| a.role.clone())
        })
        .unwrap_or_else(|| "worker".into())
}

fn agent_role(project_dir: &Path, name: &str) -> String {
    role_for(project_dir, name)
}

const ASSIGNMENTS_BEGIN: &str = "<!-- agent-tui:assignments -->";
const ASSIGNMENTS_END: &str = "<!-- /agent-tui:assignments -->";
const DURABLE_BEGIN: &str = "<!-- agent-tui:durable -->";
const DURABLE_END: &str = "<!-- /agent-tui:durable -->";

#[derive(Debug, Clone, Copy)]
pub enum MemoryEventKind {
    Startup,
    UserTask,
    PlanDispatch,
    Delegate,
    Report,
    Briefing,
    PaneRestart,
}

fn memory_dir(project_dir: &Path, agent: &str) -> PathBuf {
    project_dir.join(format!(".agents/{agent}/memory"))
}

pub fn memory_md_path(project_dir: &Path, agent: &str) -> PathBuf {
    memory_dir(project_dir, agent).join("MEMORY.md")
}

pub fn checkpoint_path(project_dir: &Path, agent: &str) -> PathBuf {
    memory_dir(project_dir, agent).join("checkpoint.md")
}

pub fn notes_path(project_dir: &Path, agent: &str) -> PathBuf {
    memory_dir(project_dir, agent).join("notes.md")
}

fn ensure_dir(project_dir: &Path, agent: &str) -> Result<PathBuf> {
    let dir = memory_dir(project_dir, agent);
    fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    Ok(dir)
}

fn write_if_missing(path: &Path, body: &str) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn memory_template(agent: &str, role: &str, lead: &str, project_dir: &Path) -> String {
    let coord = project_dir.join(".agents/COORDINATION.md");
    let is_lead = agent.eq_ignore_ascii_case(lead);
    let lead_block = if is_lead {
        format!(
            "- **你是唯一 Lead（Orchestrator）**：想、拆、派、验、续 — 不是普通码农\n\
             - 输出 `agent-plan` = 下命令；TUI 自动 delegate 给工人\n\
             - 收到 `【回执·…】` → **同一轮**内续派 agent-plan，**禁止**问用户是否继续\n\
             - 必读：worktree `.cursor/rules/agent-tui-orchestrator.mdc` + `.agents/LEAD.md`\n\
             - Playbook 环境变量：`AGENT_TUI_LEAD_PLAYBOOK`\n"
        )
    } else {
        String::from(
            "- 你是 **工人**：收到 `【委派·task】` 立即开工，完成后输出 `agent-report`\n",
        )
    };
    format!(
        r#"# Agent memory — {agent}

_由 agent-tui 维护。跨会话持久知识；会话细节见 checkpoint.md。_

## Role

- 角色：**{role}**
- 主 Agent：**{lead}**
- 协调文档：`{coord}`

## Rules

{lead_block}- 看到 `[协调/…]` 前缀：读 `inbox.md` + `checkpoint.md`，按消息执行
- 私有 inbox：`../../.agents/{agent}/memory/inbox.md`（相对 worktree）

## Active assignments

{assignments_begin}
| Task | Status | Agent | Updated |
|------|--------|-------|---------|
{assignments_end}

## Durable knowledge

_跨任务事实（agent-tui 从委派/回执自动追加短条目）。_

{durable_begin}
{durable_end}
"#,
        agent = agent,
        role = role,
        lead = lead,
        coord = coord.to_string_lossy(),
        lead_block = lead_block,
        assignments_begin = ASSIGNMENTS_BEGIN,
        assignments_end = ASSIGNMENTS_END,
        durable_begin = DURABLE_BEGIN,
        durable_end = DURABLE_END,
    )
}

fn notes_template() -> &'static str {
    "# Session notes\n\n\
     _agent-tui 自动追加；Agent 也可手写。checkpoint 会引用最近条目。_\n"
}

fn kind_label(kind: MemoryEventKind) -> &'static str {
    match kind {
        MemoryEventKind::Startup => "startup",
        MemoryEventKind::UserTask => "user-task",
        MemoryEventKind::PlanDispatch => "plan-dispatch",
        MemoryEventKind::Delegate => "delegate",
        MemoryEventKind::Report => "report",
        MemoryEventKind::Briefing => "briefing",
        MemoryEventKind::PaneRestart => "pane-restart",
    }
}

fn next_action_hint(agent: &str, role: &str, lead: &str, kind: MemoryEventKind) -> String {
    let is_lead = agent.eq_ignore_ascii_case(lead);
    match kind {
        MemoryEventKind::UserTask if is_lead => {
            "分析用户任务，输出 agent-plan JSON 代码块。".into()
        }
        MemoryEventKind::PlanDispatch if is_lead => {
            "等待工人 agent-report；收到后输出下一波 agent-plan。".into()
        }
        MemoryEventKind::Delegate if !is_lead => {
            "执行委派任务；完成后输出 agent-report 代码块。".into()
        }
        MemoryEventKind::Report if is_lead => {
            "收到回执：立即输出 agent-plan（review/续派/修复），勿问用户。".into()
        }
        MemoryEventKind::Report => "等待下一委派或协调消息。".into(),
        MemoryEventKind::Briefing if is_lead => {
            "Lead 身份已确认：读 orchestrator.mdc + LEAD.md；统筹闭环。".into()
        }
        _ => format!("以 {role} 身份响应 inbox / 协调注入。"),
    }
}

fn load_recent_inbox(agent_dir: &Path, max: usize) -> Vec<String> {
    let inbox = agent_dir.join("inbox.md");
    let Ok(content) = fs::read_to_string(inbox) else {
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .rev()
        .take(max)
        .map(String::from)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn format_checkpoint(
    agent: &str,
    role: &str,
    lead: &str,
    kind: MemoryEventKind,
    detail: &str,
    tasks: &[String],
    inbox_lines: &[String],
) -> String {
    let iso = inbox_timestamp_iso();
    let trigger = kind_label(kind);
    let next = next_action_hint(agent, role, lead, kind);
    let tasks_block = if tasks.is_empty() {
        "(none)".into()
    } else {
        tasks.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n")
    };
    let inbox_block = if inbox_lines.is_empty() {
        "(none)".into()
    } else {
        inbox_lines.join("\n")
    };
    format!(
        r#"# Session checkpoint (agent-tui)
Updated: {iso} | Agent: {agent} | Trigger: {trigger}

## §1 Active intent
{detail}

## §2 Next action
{next}

## §3 Open tasks (claims)
{tasks_block}

## §4 Recent inbox
{inbox_block}

## §5 Role context
Agent={agent}, role={role}, lead={lead}. 完整规则见 MEMORY.md 与 .agents/COORDINATION.md。

## §6 Durable candidates
_若本条事实应跨会话保留，由 Agent 或后续 dream 任务写入 MEMORY.md § Durable knowledge。_
"#
    )
}

fn append_note(agent_dir: &Path, kind: MemoryEventKind, detail: &str) -> Result<()> {
    let path = agent_dir.join("notes.md");
    let line = format!(
        "\n## [{iso} · {trigger}]\n{detail}\n",
        iso = inbox_timestamp_iso(),
        trigger = kind_label(kind),
    );
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    use std::io::Write;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn upsert_assignment_row(
    memory: &str,
    task: &str,
    status: &str,
    agent: &str,
) -> String {
    let row = format!("| {task} | {status} | {agent} | {} |", inbox_timestamp_iso());
    let (before, rest) = match memory.split_once(ASSIGNMENTS_BEGIN) {
        Some(p) => p,
        None => return memory.to_string(),
    };
    let (table, after) = match rest.split_once(ASSIGNMENTS_END) {
        Some(p) => p,
        None => return memory.to_string(),
    };

    let mut lines: Vec<String> = table
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    if lines.len() < 2 {
        lines = vec![
            "| Task | Status | Agent | Updated |".into(),
            "|------|--------|-------|---------|".into(),
        ];
    }

    let task_lc = task.to_lowercase();
    lines.retain(|l| {
        if !l.starts_with('|') || l.contains("Task |") || l.contains("---") {
            return true;
        }
        !l.to_lowercase().contains(&format!("| {task_lc} "))
            && !l.to_lowercase().starts_with(&format!("| {task_lc} |"))
    });

    if status != "released" {
        lines.push(row);
    }

    format!(
        "{before}{ASSIGNMENTS_BEGIN}\n{}\n{ASSIGNMENTS_END}{after}",
        lines.join("\n")
    )
}

fn append_durable_line(memory: &str, line: &str) -> String {
    let (before, rest) = match memory.split_once(DURABLE_BEGIN) {
        Some(p) => p,
        None => {
            return format!(
                "{memory}\n\n## Durable knowledge\n\n{DURABLE_BEGIN}\n- [{iso}] {line}\n{DURABLE_END}\n",
                iso = inbox_timestamp_iso()
            );
        }
    };
    let (mid, after) = match rest.split_once(DURABLE_END) {
        Some(p) => p,
        None => return memory.to_string(),
    };
    format!(
        "{before}{DURABLE_BEGIN}{mid}- [{iso}] {line}\n{DURABLE_END}{after}",
        iso = inbox_timestamp_iso()
    )
}

fn update_memory_assignments(
    project_dir: &Path,
    memory_owner: &str,
    task: &str,
    status: &str,
    assignee: &str,
    durable_line: Option<&str>,
) -> Result<()> {
    let path = memory_md_path(project_dir, memory_owner);
    let Ok(mut content) = fs::read_to_string(&path) else {
        return Ok(());
    };
    content = upsert_assignment_row(&content, task, status, assignee);
    if let Some(line) = durable_line {
        content = append_durable_line(&content, line);
    }
    fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Create MEMORY.md / notes.md templates for one agent (never overwrites existing MEMORY).
pub fn ensure_agent_memory_files(
    project_dir: &Path,
    agent: &str,
    role: &str,
    lead: &str,
) -> Result<()> {
    let dir = ensure_dir(project_dir, agent)?;
    write_if_missing(
        &memory_md_path(project_dir, agent),
        &memory_template(agent, role, lead, project_dir),
    )?;
    write_if_missing(&dir.join("notes.md"), notes_template())?;
    Ok(())
}

/// Initialize memory files for every configured agent.
pub fn ensure_all_agent_memory(
    project_dir: &Path,
    agents: &[(String, String)],
    lead: &str,
) -> Result<()> {
    for (name, role) in agents {
        ensure_agent_memory_files(project_dir, name, role, lead)?;
    }
    Ok(())
}

/// Record a coordination event: append notes, rewrite checkpoint, optionally update MEMORY assignments.
pub fn record_event(
    project_dir: &Path,
    agent: &str,
    role: &str,
    lead: &str,
    kind: MemoryEventKind,
    detail: &str,
    task: Option<(&str, &str, &str)>,
) -> Result<()> {
    ensure_agent_memory_files(project_dir, agent, role, lead)?;
    let dir = memory_dir(project_dir, agent);

    append_note(&dir, kind, detail)?;

    let snapshot = claims::load_claims_snapshot(project_dir);
    let tasks = snapshot
        .agent_tasks
        .get(agent)
        .cloned()
        .unwrap_or_default();
    let inbox = load_recent_inbox(&dir, 6);

    let checkpoint = format_checkpoint(agent, role, lead, kind, detail, &tasks, &inbox);
    fs::write(checkpoint_path(project_dir, agent), checkpoint)
        .with_context(|| format!("write checkpoint for {agent}"))?;

    if let Some((task_name, status, assignee)) = task {
        let durable = match kind {
            MemoryEventKind::Delegate if agent.eq_ignore_ascii_case(assignee) => {
                Some(format!("收到委派任务「{task_name}」"))
            }
            MemoryEventKind::Delegate => Some(format!("已委派 @{assignee} 任务「{task_name}」")),
            MemoryEventKind::Report if agent.eq_ignore_ascii_case(lead) => Some(format!(
                "收到 @{assignee} 回执「{task_name}」({status})"
            )),
            MemoryEventKind::Report => {
                Some(format!("回执「{task_name}」status={status}"))
            }
            _ => None,
        };
        update_memory_assignments(
            project_dir,
            agent,
            task_name,
            status,
            assignee,
            durable.as_deref(),
        )?;
    }

    let _ = crate::memory_fts::index_agent(project_dir, agent);
    Ok(())
}

pub fn on_startup(project_dir: &Path, agents: &[(String, String)], lead: &str) -> Result<()> {
    ensure_all_agent_memory(project_dir, agents, lead)?;
    for (name, role) in agents {
        let detail = format!("agent-tui 已启动；Agent={name}，role={role}。");
        record_event(
            project_dir,
            name,
            role,
            lead,
            MemoryEventKind::Startup,
            &detail,
            None,
        )?;
    }
    Ok(())
}

pub fn on_user_task(project_dir: &Path, lead: &str, message: &str) -> Result<()> {
    let lead_role = agent_role(project_dir, lead);
    record_event(
        project_dir,
        lead,
        &lead_role,
        lead,
        MemoryEventKind::UserTask,
        message,
        None,
    )
}

pub fn on_plan_dispatch(project_dir: &Path, lead: &str, summary: &str) -> Result<()> {
    let lead_role = agent_role(project_dir, lead);
    record_event(
        project_dir,
        lead,
        &lead_role,
        lead,
        MemoryEventKind::PlanDispatch,
        summary,
        None,
    )
}

pub fn on_delegate(
    project_dir: &Path,
    lead: &str,
    worker: &str,
    task: &str,
    description: &str,
) -> Result<()> {
    let worker_role = agent_role(project_dir, worker);
    let worker_detail = format!(
        "【委派·{task}】来自 {lead}。{desc}",
        desc = if description.trim().is_empty() {
            "请立即执行。"
        } else {
            description.trim()
        }
    );
    record_event(
        project_dir,
        worker,
        &worker_role,
        lead,
        MemoryEventKind::Delegate,
        &worker_detail,
        Some((task, "in_progress", worker)),
    )?;

    let lead_detail = format!("已委派 @{worker} 任务「{task}」。");
    let lead_role = agent_role(project_dir, lead);
    record_event(
        project_dir,
        lead,
        &lead_role,
        lead,
        MemoryEventKind::Delegate,
        &lead_detail,
        Some((task, "delegated", worker)),
    )?;
    Ok(())
}

pub fn on_report(
    project_dir: &Path,
    reporter: &str,
    lead: &str,
    task: &str,
    status: &str,
    summary: &str,
) -> Result<()> {
    let reporter_role = agent_role(project_dir, reporter);
    let reporter_detail = format!("已回执「{task}」({status})：{summary}");
    record_event(
        project_dir,
        reporter,
        &reporter_role,
        lead,
        MemoryEventKind::Report,
        &reporter_detail,
        Some((task, status, reporter)),
    )?;

    let lead_detail = format!("收到 {reporter} 回执「{task}」({status})：{summary}");
    let lead_role = agent_role(project_dir, lead);
    record_event(
        project_dir,
        lead,
        &lead_role,
        lead,
        MemoryEventKind::Report,
        &lead_detail,
        Some((task, status, reporter)),
    )?;
    Ok(())
}

/// Refresh Lead ## Rules in MEMORY.md after briefing / identity sync.
pub fn refresh_lead_identity(project_dir: &Path, lead: &str) -> Result<()> {
    let path = memory_md_path(project_dir, lead);
    if !path.is_file() {
        return Ok(());
    }
    let role = agent_role(project_dir, lead);
    let fresh = memory_template(lead, &role, lead, project_dir);
    let fresh_rules = extract_section(&fresh, "## Rules", "## Active assignments");
    let mut content = fs::read_to_string(&path).with_context(|| path.display().to_string())?;
    if let Some(replaced) = replace_section(
        &content,
        "## Rules",
        "## Active assignments",
        &fresh_rules,
    ) {
        content = replaced;
        fs::write(&path, content).with_context(|| path.display().to_string())?;
    }
    Ok(())
}

fn extract_section(text: &str, begin: &str, end: &str) -> String {
    let start = text.find(begin).unwrap_or(0);
    let rest = &text[start..];
    let end_off = rest.find(end).unwrap_or(rest.len());
    rest[..end_off].trim_end().to_string() + "\n\n"
}

fn replace_section(text: &str, begin: &str, end: &str, new_body: &str) -> Option<String> {
    let start = text.find(begin)?;
    let rest = &text[start..];
    let end_off = rest.find(end)?;
    let before = &text[..start];
    let after = &rest[end_off..];
    Some(format!("{before}{new_body}{after}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_same_task() {
        let mem = memory_template("codex", "executor", "cursor", Path::new(r"D:\proj"));
        let updated = upsert_assignment_row(&mem, "auth-api", "in_progress", "codex");
        assert!(updated.contains("auth-api"));
        let updated2 = upsert_assignment_row(&updated, "auth-api", "done", "codex");
        assert!(updated2.contains("| auth-api | done |"));
        assert!(!updated2.contains("| auth-api | in_progress |"));
    }

    #[test]
    fn checkpoint_contains_sections() {
        let cp = format_checkpoint(
            "cursor",
            "architect",
            "cursor",
            MemoryEventKind::UserTask,
            "实现登录",
            &["auth-api".into()],
            &["[12:00:00] user: hi".into()],
        );
        assert!(cp.contains("§1 Active intent"));
        assert!(cp.contains("§4 Recent inbox"));
    }
}
