use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default)]
pub struct AgentMeta {
    pub branch: String,
    pub unread: u32,
    pub tasks: Vec<String>,
    pub claim_conflict: bool,
}

#[derive(Debug, Clone)]
pub struct NotifyEvent {
    pub line_no: usize,
    pub agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordEvent {
    pub line_no: usize,
    pub time: Option<String>,
    pub kind: String,
    pub agent: Option<String>,
    pub message: String,
    pub from: String,
    pub task: Option<String>,
    pub action: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EventRecord {
    #[serde(rename = "time")]
    _time: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    agent: Option<String>,
    #[serde(rename = "message")]
    message: Option<String>,
    from: Option<String>,
    task: Option<String>,
    action: Option<String>,
}

pub fn branch_for_worktree(worktree: &Path) -> String {
    let branch_file = worktree
        .parent()
        .map(|p| p.join("branch.txt"))
        .unwrap_or_else(|| PathBuf::from("branch.txt"));
    fs::read_to_string(branch_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "-".into())
}

pub fn load_agent_meta(
    worktree: &Path,
    unread: u32,
    tasks: Vec<String>,
    claim_conflict: bool,
) -> AgentMeta {
    AgentMeta {
        branch: branch_for_worktree(worktree),
        unread,
        tasks,
        claim_conflict,
    }
}

pub fn shared_inbox_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/inbox.md")
}

pub fn events_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/events.jsonl")
}

pub fn agent_inbox_path(project_dir: &Path, agent: &str) -> PathBuf {
    project_dir.join(format!(".agents/{agent}/memory/inbox.md"))
}

/// Last few non-empty inbox lines, joined for a one-line status strip.
pub fn inbox_snippet(project_dir: &Path, max_lines: usize) -> String {
    let lines = load_shared_inbox_lines(project_dir);
    if lines.is_empty() {
        return String::from("（空）");
    }
    let start = lines.len().saturating_sub(max_lines);
    let snippet = lines[start..].join("  │  ");
    truncate_chars(&snippet, 120)
}

pub fn load_shared_inbox_lines(project_dir: &Path) -> Vec<String> {
    let path = shared_inbox_path(project_dir);
    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !is_inbox_noise_line(l))
        .map(normalize_inbox_display_line)
        .collect()
}

fn is_inbox_noise_line(line: &str) -> bool {
    line.contains("请知悉") || line.contains("其他 Agent 请知悉")
}

/// Collapse legacy double timestamps like `[t] user: [t] body` → `[t] body`.
fn normalize_inbox_display_line(line: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^\[([0-9]{2}:[0-9]{2}:[0-9]{2})\] \S+: \[([0-9]{2}:[0-9]{2}:[0-9]{2})\] (.*)$")
            .expect("inbox normalize regex")
    });
    if let Some(caps) = re.captures(line) {
        let t1 = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let t2 = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let body = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        if t1 == t2 {
            return format!("[{t1}] {body}");
        }
    }
    line.to_string()
}

pub fn load_notify_events(project_dir: &Path) -> Vec<NotifyEvent> {
    load_coord_events(project_dir)
        .into_iter()
        .filter(|e| e.kind == "notify")
        .filter_map(|e| {
            Some(NotifyEvent {
                line_no: e.line_no,
                agent: e.agent?,
            })
        })
        .collect()
}

pub fn load_coord_events(project_dir: &Path) -> Vec<CoordEvent> {
    let path = events_path(project_dir);
    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| parse_coord_line(idx + 1, line))
        .collect()
}

fn parse_coord_line(line_no: usize, line: &str) -> Option<CoordEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let rec: EventRecord = serde_json::from_str(line).ok()?;
    match rec.kind.as_str() {
        "notify" => {
            let agent = rec.agent?;
            let message = rec.message.unwrap_or_default();
            if message.is_empty() {
                return None;
            }
            Some(CoordEvent {
                line_no,
                time: rec._time,
                kind: rec.kind,
                agent: Some(agent),
                message,
                from: rec.from.unwrap_or_else(|| "user".into()),
                task: None,
                action: None,
            })
        }
        "broadcast" => {
            let message = rec.message?;
            Some(CoordEvent {
                line_no,
                time: rec._time,
                kind: rec.kind,
                agent: None,
                message,
                from: rec.from.unwrap_or_else(|| "user".into()),
                task: None,
                action: None,
            })
        }
        "claim" => {
            let agent = rec.agent?;
            let task = rec.task?;
            Some(CoordEvent {
                line_no,
                time: rec._time,
                kind: rec.kind,
                agent: Some(agent),
                message: rec.message.unwrap_or_default(),
                from: rec.from.unwrap_or_else(|| "system".into()),
                task: Some(task),
                action: rec.action,
            })
        }
        _ => None,
    }
}

pub fn unread_for_agent(events: &[NotifyEvent], agent: &str, ack_line: usize) -> u32 {
    events
        .iter()
        .filter(|e| e.agent == agent && e.line_no > ack_line)
        .count() as u32
}

pub fn compute_unread(
    agent_names: &[String],
    events: &[NotifyEvent],
    ack_lines: &[usize],
) -> Vec<u32> {
    agent_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let ack = ack_lines.get(i).copied().unwrap_or(0);
            unread_for_agent(events, name, ack)
        })
        .collect()
}

fn ensure_shared_dirs(project_dir: &Path, agent: Option<&str>) -> Result<()> {
    fs::create_dir_all(project_dir.join(".agents/shared"))
        .context("create .agents/shared")?;
    if let Some(name) = agent {
        fs::create_dir_all(project_dir.join(format!(".agents/{name}/memory")))
            .context("create agent memory dir")?;
    }
    Ok(())
}

fn display_user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".into())
}

pub(crate) fn inbox_timestamp_short() -> String {
    chrono_lite_now("%H:%M:%S")
}

pub(crate) fn inbox_timestamp_iso() -> String {
    chrono_lite_now("%Y-%m-%dT%H:%M:%S")
}

/// Minimal local time formatting without adding chrono dependency.
fn chrono_lite_now(_fmt: &str) -> String {
    // Use system time formatted via standard library where possible.
    use std::time::SystemTime;
    let _now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Good enough for inbox: HH:MM:SS from local offset approximation via local time on Windows.
    #[cfg(windows)]
    {
        use std::mem::MaybeUninit;
        #[repr(C)]
        struct SystemTimeWin {
            w_year: u16,
            w_month: u16,
            w_day_of_week: u16,
            w_day: u16,
            w_hour: u16,
            w_minute: u16,
            w_second: u16,
            w_milliseconds: u16,
        }
        extern "system" {
            fn GetLocalTime(lpSystemTime: *mut SystemTimeWin);
        }
        let mut st = MaybeUninit::<SystemTimeWin>::uninit();
        // SAFETY:
        // - `SystemTimeWin` is `#[repr(C)]` with 8×u16 fields, layout-compatible
        //   with the Win32 `SYSTEMTIME` struct (16 bytes, no padding).
        // - `GetLocalTime` is a documented kernel32 function that *always* writes
        //   every field (no return value, no failure mode). It is thread-safe per
        //   Microsoft docs, so no external synchronisation is required.
        // - After the call the struct is fully initialised, so
        //   `MaybeUninit::assume_init` is valid.
        // - Alternative considered: `std::time::SystemTime` is UTC-only and would
        //   need `chrono` for local offset; this FFI keeps the crate dep-free.
        unsafe {
            GetLocalTime(st.as_mut_ptr());
            let st = st.assume_init();
            if _fmt.contains('T') {
                return format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                    st.w_year, st.w_month, st.w_day, st.w_hour, st.w_minute, st.w_second
                );
            }
            format!("{:02}:{:02}:{:02}", st.w_hour, st.w_minute, st.w_second)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = _now;
        "00:00:00".into()
    }
}

pub fn append_shared_message(project_dir: &Path, message: &str) -> Result<()> {
    broadcast_message(project_dir, message, "user")
}

/// System/coordination line: `[HH:MM:SS] body` — no extra user prefix, no broadcast relay.
pub fn append_shared_line(project_dir: &Path, body: &str) -> Result<()> {
    let body = body.trim();
    if body.is_empty() {
        return Ok(());
    }
    ensure_shared_dirs(project_dir, None)?;
    let line = format!("[{}] {}\n", inbox_timestamp_short(), body);
    let path = shared_inbox_path(project_dir);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

pub fn broadcast_message(project_dir: &Path, message: &str, from: &str) -> Result<()> {
    let msg = message.trim();
    if msg.is_empty() {
        return Ok(());
    }
    ensure_shared_dirs(project_dir, None)?;
    let user = display_user();
    let line = if from == "user" || from.eq_ignore_ascii_case(&user) {
        format!("[{}] {}: {}\n", inbox_timestamp_short(), user, msg)
    } else {
        format!("[{}] {}: {}\n", inbox_timestamp_short(), from, msg)
    };
    let path = shared_inbox_path(project_dir);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(line.as_bytes())?;
    append_coord_event(
        project_dir,
        "broadcast",
        None,
        msg,
        from,
        None,
        None,
    )?;
    Ok(())
}

pub fn notify_agent(project_dir: &Path, agent: &str, message: &str) -> Result<()> {
    notify_agent_from(project_dir, agent, message, "user")
}

fn write_agent_inbox_lines(
    project_dir: &Path,
    agent: &str,
    msg: &str,
    from: &str,
) -> Result<()> {
    ensure_shared_dirs(project_dir, Some(agent))?;

    let user = display_user();
    let short = inbox_timestamp_short();

    let shared_line = if from.eq_ignore_ascii_case("user") || from == user {
        format!("[{short}] {user} → {agent}: {msg}\n")
    } else {
        format!("[{short}] {from} → {agent}: {msg}\n")
    };
    let private_line = format!("[{short}] {from}: {msg}\n");

    let shared_path = shared_inbox_path(project_dir);
    let mut shared = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&shared_path)
        .with_context(|| format!("open {}", shared_path.display()))?;
    shared.write_all(shared_line.as_bytes())?;

    let private_path = agent_inbox_path(project_dir, agent);
    let mut private = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&private_path)
        .with_context(|| format!("open {}", private_path.display()))?;
    private.write_all(private_line.as_bytes())?;
    Ok(())
}

pub fn notify_agent_from(
    project_dir: &Path,
    agent: &str,
    message: &str,
    from: &str,
) -> Result<()> {
    let msg = message.trim();
    if msg.is_empty() {
        return Ok(());
    }
    write_agent_inbox_lines(project_dir, agent, msg, from)?;
    append_coord_event(project_dir, "notify", Some(agent), msg, from, None, None)?;
    Ok(())
}

/// Write inbox (shared + private) without events.jsonl — avoids Relay PTY duplicate inject.
pub fn notify_agent_inbox_only(
    project_dir: &Path,
    agent: &str,
    message: &str,
    from: &str,
) -> Result<()> {
    let msg = message.trim();
    if msg.is_empty() {
        return Ok(());
    }
    write_agent_inbox_lines(project_dir, agent, msg, from)
}

/// Write inbox lines only — no events.jsonl, no Relay PTY injection.
/// Use for low-priority team awareness so other agents don't auto-reply.
pub fn append_agent_inbox_notice(
    project_dir: &Path,
    agent: &str,
    message: &str,
    from: &str,
) -> Result<()> {
    let msg = message.trim();
    if msg.is_empty() {
        return Ok(());
    }
    ensure_shared_dirs(project_dir, Some(agent))?;

    let short = inbox_timestamp_short();
    let private_line = format!("[{short}] {from}: {msg}\n");

    let private_path = agent_inbox_path(project_dir, agent);
    let mut private = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&private_path)
        .with_context(|| format!("open {}", private_path.display()))?;
    private.write_all(private_line.as_bytes())?;
    Ok(())
}

pub fn append_coord_event(
    project_dir: &Path,
    kind: &str,
    agent: Option<&str>,
    message: &str,
    from: &str,
    task: Option<&str>,
    action: Option<&str>,
) -> Result<()> {
    ensure_shared_dirs(project_dir, agent)?;
    let iso = inbox_timestamp_iso();
    let mut event = serde_json::json!({
        "time": iso,
        "type": kind,
        "message": message,
        "from": from,
    });
    if let Some(a) = agent {
        event["agent"] = serde_json::Value::String(a.into());
    }
    if let Some(t) = task {
        event["task"] = serde_json::Value::String(t.into());
    }
    if let Some(act) = action {
        event["action"] = serde_json::Value::String(act.into());
    }
    let events_path = events_path(project_dir);
    let mut events = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_path)
        .with_context(|| format!("open {}", events_path.display()))?;
    writeln!(events, "{event}")?;
    Ok(())
}

pub fn validate_task_name(task: &str) -> Result<()> {
    let t = task.trim();
    if t.is_empty() {
        bail!("任务名不能为空");
    }
    if t.len() > 48 {
        bail!("任务名过长（最多 48 字符）");
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("任务名仅允许字母、数字、-、_");
    }
    Ok(())
}

/// Parse inbox input. Priority: !任务 > !委派/!回执 > !认领/!释放 > @agent > broadcast.
pub enum InboxCommand<'a> {
    /// High-level user task for lead agent to decompose.
    TaskToLead {
        message: &'a str,
    },
    Delegate {
        worker: &'a str,
        task: &'a str,
        description: &'a str,
    },
    Report {
        lead: &'a str,
        task: Option<&'a str>,
        message: &'a str,
    },
    Claim {
        agent: &'a str,
        task: &'a str,
    },
    Release {
        agent: &'a str,
        task: &'a str,
    },
    Notify {
        agent: &'a str,
        body: &'a str,
    },
    Broadcast(&'a str),
}

pub fn parse_inbox_command<'a>(
    raw: &'a str,
    agent_names: &[String],
    default_agent: Option<&'a str>,
    lead_agent: &'a str,
) -> Result<InboxCommand<'a>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("消息不能为空");
    }

    if let Some(message) = parse_task_to_lead(trimmed) {
        return Ok(InboxCommand::TaskToLead { message });
    }
    if let Some(cmd) = parse_delegate_command(trimmed, agent_names) {
        return Ok(cmd);
    }
    if let Some(cmd) = parse_report_command(trimmed, agent_names, lead_agent) {
        return Ok(cmd);
    }

    if let Some(cmd) = parse_claim_command(trimmed, agent_names, default_agent, true) {
        return Ok(cmd);
    }
    if let Some(cmd) = parse_claim_command(trimmed, agent_names, default_agent, false) {
        return Ok(cmd);
    }

    let (target, body) = parse_directed_message(agent_names, trimmed);
    if let Some(agent) = target {
        if body.is_empty() {
            bail!("@{} 后需要跟消息内容", agent);
        }
        return Ok(InboxCommand::Notify { agent, body });
    }

    Ok(InboxCommand::Broadcast(trimmed))
}

fn parse_task_to_lead(trimmed: &str) -> Option<&str> {
    for p in ["!任务", "!task", "!需求"] {
        if let Some(rest) = trimmed.strip_prefix(p) {
            let msg = rest.trim();
            if !msg.is_empty() {
                return Some(msg);
            }
        }
    }
    None
}

fn parse_delegate_command<'a>(
    trimmed: &'a str,
    agent_names: &[String],
) -> Option<InboxCommand<'a>> {
    let prefixes = ["!委派", "!delegate", "@dispatch"];
    let mut rest = None;
    for p in prefixes {
        if let Some(r) = trimmed.strip_prefix(p) {
            rest = Some(r.trim());
            break;
        }
    }
    let rest = rest?;
    let after_at = rest
        .strip_prefix('@')
        .map(str::trim)
        .unwrap_or(rest);
    let (worker, after_worker) = after_at
        .split_once(char::is_whitespace)
        .map(|(n, t)| (n.trim(), t.trim()))
        .unwrap_or((after_at.trim(), ""));
    if !agent_names.iter().any(|a| a == worker) {
        return None;
    }
    let (task, description) = split_task_and_description(after_worker).ok()?;
    Some(InboxCommand::Delegate {
        worker,
        task,
        description,
    })
}

fn parse_report_command<'a>(
    trimmed: &'a str,
    agent_names: &[String],
    default_lead: &'a str,
) -> Option<InboxCommand<'a>> {
    let prefixes = ["!回执", "!report"];
    let mut rest = None;
    for p in prefixes {
        if let Some(r) = trimmed.strip_prefix(p) {
            rest = Some(r.trim());
            break;
        }
    }
    let mut rest = rest?;
    if rest.is_empty() {
        return None;
    }

    let mut lead = default_lead;
    if let Some(r) = rest.strip_prefix('@') {
        let (name, after) = r
            .split_once(char::is_whitespace)
            .map(|(n, t)| (n.trim(), t.trim()))
            .unwrap_or((r.trim(), ""));
        if agent_names.iter().any(|a| a == name) {
            lead = name;
            rest = after;
        }
    }
    if rest.is_empty() {
        return None;
    }

    let (task, message) = match split_task_and_description(rest) {
        Ok((task, msg)) if !msg.is_empty() => (Some(task), msg),
        Ok((task, _)) => (Some(task), "已完成"),
        Err(_) => (None, rest),
    };
    Some(InboxCommand::Report {
        lead,
        task,
        message,
    })
}

fn split_task_and_description(rest: &str) -> Result<(&str, &str)> {
    let rest = rest.trim();
    if rest.is_empty() {
        bail!("缺少任务名");
    }
    let (first, tail) = rest
        .split_once(char::is_whitespace)
        .map(|(a, b)| (a.trim(), b.trim()))
        .unwrap_or((rest, ""));
    validate_task_name(first)?;
    Ok((first, tail))
}

fn parse_claim_command<'a>(
    trimmed: &'a str,
    agent_names: &[String],
    default_agent: Option<&'a str>,
    claim: bool,
) -> Option<InboxCommand<'a>> {
    let prefixes = if claim {
        ["!认领", "!claim"]
    } else {
        ["!释放", "!release"]
    };
    let mut rest = None;
    for p in prefixes {
        if let Some(r) = trimmed.strip_prefix(p) {
            rest = Some(r.trim());
            break;
        }
    }
    let rest = rest?;

    let (agent, task) = if let Some(r) = rest.strip_prefix('@') {
        let (name, task) = r
            .split_once(char::is_whitespace)
            .map(|(n, t)| (n.trim(), t.trim()))
            .unwrap_or((r.trim(), ""));
        if !agent_names.iter().any(|a| a == name) {
            return None;
        }
        (name, task)
    } else if let Some(name) = default_agent {
        (name, rest)
    } else {
        return None;
    };

    if task.is_empty() {
        return None;
    }

    if claim {
        Some(InboxCommand::Claim { agent, task })
    } else {
        Some(InboxCommand::Release { agent, task })
    }
}

/// Parse `@agent message` prefix; returns (target, body). No prefix => broadcast.
pub fn parse_directed_message<'a>(
    agent_names: &[String],
    raw: &'a str,
) -> (Option<&'a str>, &'a str) {
    let trimmed = raw.trim();
    if !trimmed.starts_with('@') {
        return (None, trimmed);
    }
    let rest = &trimmed[1..];
    let (name, body) = rest
        .split_once(char::is_whitespace)
        .map(|(n, b)| (n, b.trim()))
        .unwrap_or((rest, ""));
    if agent_names.iter().any(|a| a == name) {
        (Some(name), body)
    } else {
        (None, trimmed)
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> Vec<String> {
        vec![
            "claude".into(),
            "codex".into(),
            "mimo".into(),
            "kimi".into(),
        ]
    }

    #[test]
    fn normalize_doubled_timestamp() {
        let line = "[02:21:13] zhugu: [02:21:13] claude 认领任务「auth-api」";
        assert_eq!(
            normalize_inbox_display_line(line),
            "[02:21:13] claude 认领任务「auth-api」"
        );
    }

    #[test]
    fn parse_directed() {
        let agents = vec!["claude".into(), "mimo".into()];
        let (t, b) = parse_directed_message(&agents, "@claude please review");
        assert_eq!(t, Some("claude"));
        assert_eq!(b, "please review");
        let (t, b) = parse_directed_message(&agents, "hello all");
        assert_eq!(t, None);
        assert_eq!(b, "hello all");
    }

    #[test]
    fn parse_delegate_and_report() {
        let a = agents();
        let cmd = parse_inbox_command(
            "!委派 @codex auth-api 实现登录",
            &a,
            None,
            "claude",
        )
        .unwrap();
        match cmd {
            InboxCommand::Delegate {
                worker,
                task,
                description,
            } => {
                assert_eq!(worker, "codex");
                assert_eq!(task, "auth-api");
                assert_eq!(description, "实现登录");
            }
            _ => panic!("expected delegate"),
        }

        let cmd = parse_inbox_command("!回执 auth-api 已完成", &a, None, "claude").unwrap();
        match cmd {
            InboxCommand::Report {
                lead,
                task,
                message,
            } => {
                assert_eq!(lead, "claude");
                assert_eq!(task, Some("auth-api"));
                assert_eq!(message, "已完成");
            }
            _ => panic!("expected report"),
        }
    }
}
