use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Deserialize;

pub const PREFERRED_ORDER: &[&str] = &["cursor", "claude", "codex", "mimo", "kimi"];

#[derive(Debug, Clone, Deserialize)]
pub struct AgentsFile {
    pub agents: BTreeMap<String, AgentEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentEntry {
    pub command: String,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_role() -> String {
    "worker".into()
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub name: String,
    pub command: String,
    pub role: String,
    pub worktree: PathBuf,
}

pub fn validate_agent_name(name: &str) -> Result<()> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[a-zA-Z0-9_-]{1,32}$").unwrap());
    if re.is_match(name) {
        Ok(())
    } else {
        bail!("invalid agent name: {name}")
    }
}

pub fn resolve_project_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let dir = if let Some(dir) = explicit {
        dir.canonicalize().unwrap_or(dir)
    } else if let Ok(env) = std::env::var("AGENT_TUI_PROJECT") {
        let p = PathBuf::from(env.trim());
        if agents_yaml_path(&p).is_some() {
            p
        } else {
            bail!(
                "AGENT_TUI_PROJECT 无效（找不到 .agents/agents.yaml）：{}",
                p.display()
            );
        }
    } else {
        let mut dir = std::env::current_dir().context("current directory")?;
        loop {
            if agents_yaml_path(&dir).is_some() {
                break dir;
            }
            if !dir.pop() {
                bail!("could not find .agents/agents.yaml or agents.yaml; use --project-dir");
            }
        }
    };
    Ok(normalize_windows_path(dir))
}

/// Strip `\\?\` extended-length prefix; CMD.EXE and some CLIs reject it as cwd.
pub fn normalize_windows_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

pub fn worktree_cwd(project: &Path, agent: &str) -> PathBuf {
    normalize_windows_path(project.join(".agents").join(agent).join("worktree"))
}

pub fn agents_yaml_path(project: &Path) -> Option<PathBuf> {
    let nested = project.join(".agents").join("agents.yaml");
    if nested.is_file() {
        return Some(nested);
    }
    let root = project.join("agents.yaml");
    if root.is_file() {
        return Some(root);
    }
    None
}

pub fn load_agents(project: &Path) -> Result<Vec<AgentSpec>> {
    let yaml_path = agents_yaml_path(project)
        .with_context(|| format!("no agents.yaml under {}", project.display()))?;
    let text = fs::read_to_string(&yaml_path)
        .with_context(|| format!("read {}", yaml_path.display()))?;
    let file: AgentsFile = serde_yaml::from_str(&text).context("parse agents.yaml")?;

    let mut enabled: Vec<(String, AgentEntry)> = file
        .agents
        .into_iter()
        .filter(|(name, entry)| {
            if let Err(e) = validate_agent_name(name) {
                crate::terminal::log_message(
                    project,
                    "warn",
                    &format!("skip agent {name}: {e}"),
                );
                false
            } else {
                entry.enabled
            }
        })
        .collect();

    enabled.sort_by(|a, b| {
        let ia = PREFERRED_ORDER.iter().position(|k| *k == a.0);
        let ib = PREFERRED_ORDER.iter().position(|k| *k == b.0);
        match (ia, ib) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.cmp(&b.0),
        }
    });

    Ok(enabled
        .into_iter()
        .map(|(name, entry)| AgentSpec {
            worktree: worktree_cwd(project, &name),
            name,
            command: entry.command,
            role: entry.role,
        })
        .collect())
}

/// Primary coordinator: `role: architect`, else `AGENT_TUI_LEAD`, else `cursor`, else `claude`, else first.
pub fn resolve_lead_agent(agents: &[AgentSpec]) -> String {
    if let Ok(name) = std::env::var("AGENT_TUI_LEAD") {
        let name = name.trim();
        if !name.is_empty()
            && agents.iter().any(|a| a.name.eq_ignore_ascii_case(name))
        {
            return name.to_string();
        }
    }
    agents
        .iter()
        .find(|a| a.role.eq_ignore_ascii_case("architect"))
        .or_else(|| agents.iter().find(|a| a.name == "cursor"))
        .or_else(|| agents.iter().find(|a| a.name == "claude"))
        .or_else(|| agents.first())
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "cursor".into())
}

pub fn parse_command(command: &str) -> Result<(String, Vec<String>)> {
    let parts = shell_words::split(command).context("parse agent command")?;
    let Some(program) = parts.first() else {
        bail!("empty command");
    };
    Ok((program.clone(), parts[1..].to_vec()))
}

/// Build a portable-pty command, preferring direct `node` launch for npm CLIs on Windows.
pub fn build_command(command: &str) -> Result<portable_pty::CommandBuilder> {
    use portable_pty::CommandBuilder;

    let (program, args) = parse_command(command)?;

    #[cfg(windows)]
    if program == "agent" || program == "cursor-agent" {
        return build_cursor_agent_command(&args);
    }

    #[cfg(windows)]
    if program == "cursor" {
        if args.first().is_some_and(|a| a == "agent") {
            return build_cursor_agent_command(&args[1..]);
        }
        return build_cursor_command(&args);
    }

    #[cfg(windows)]
    if let Some(mut cmd) = try_build_node_npm_command(&program, &args) {
        apply_pty_env(&mut cmd);
        return Ok(cmd);
    }

    let mut cmd = CommandBuilder::new(&program);
    for arg in &args {
        cmd.arg(arg);
    }
    apply_pty_env(&mut cmd);

    #[cfg(windows)]
    if windows_needs_cmd_wrapper(&program) {
        return Ok(wrap_cmd_shim(&program, &args));
    }

    Ok(cmd)
}

/// Set terminal geometry env vars after the PTY size is known.
pub fn apply_pty_size(cmd: &mut portable_pty::CommandBuilder, rows: u16, cols: u16) {
    cmd.env("LINES", rows.to_string());
    cmd.env("COLUMNS", cols.to_string());
}

/// Tell spawned CLIs they run inside agent-tui multi-agent grid.
pub fn apply_agent_tui_env(
    cmd: &mut portable_pty::CommandBuilder,
    project_dir: &Path,
    agent_name: &str,
    role: &str,
    lead_agent: &str,
) {
    cmd.env("AGENT_TUI", "1");
    cmd.env(
        "AGENT_TUI_PROJECT",
        normalize_windows_path(project_dir.to_path_buf())
            .to_string_lossy()
            .to_string(),
    );
    cmd.env("AGENT_TUI_AGENT", agent_name);
    cmd.env("AGENT_TUI_ROLE", role);
    cmd.env("AGENT_TUI_LEAD", lead_agent);
    cmd.env(
        "AGENT_TUI_COORD_DOC",
        normalize_windows_path(project_dir.join(".agents/COORDINATION.md"))
            .to_string_lossy()
            .to_string(),
    );
    if agent_name.eq_ignore_ascii_case(lead_agent) {
        cmd.env("AGENT_TUI_ORCHESTRATOR", "1");
    }
}

/// Install Cursor rules + AGENTS stub so the lead CLI loads orchestration context from cwd.
pub fn sync_lead_coordination_rules(project_dir: &Path, worktree: &Path) -> Result<()> {
    let coord = normalize_windows_path(project_dir.join(".agents/COORDINATION.md"));
    let coord_s = coord.to_string_lossy();
    let rules_dir = worktree.join(".cursor/rules");
    fs::create_dir_all(&rules_dir).with_context(|| format!("mkdir {}", rules_dir.display()))?;

    let rules_body = format!(
        r#"---
description: agent-tui multi-agent orchestrator (auto-installed)
alwaysApply: true
---

# agent-tui 主 Agent（协调者）

你在 Windows **agent-tui** 五宫格 TUI 中担任 **主 Agent**。环境变量 `AGENT_TUI_ORCHESTRATOR=1`。

## 闭环（全自动，勿等用户确认）

1. 用户通过 `!任务` 或留言板提交需求
2. 你拆解并输出 **agent-plan** 代码块 → TUI 自动派发给工人
   - **推荐**：同时写入 `.agents/shared/plan_inbox.jsonl`（更可靠，见 COORDINATION.md）
3. 工人输出 **agent-report** → TUI 自动回传给你
4. 你输出下一波 **agent-plan**（验收 / 修复 / 续派）

## agent-plan 格式（PTY + 可选文件双通道）

终端输出：
```agent-plan
[{{"worker":"codex","task":"auth-api","description":"实现登录 API","depends_on":[]}}]
```

或追加一行到 `../../.agents/shared/plan_inbox.jsonl`（相对 worktree）：
```json
{{"lead":"cursor","worker":"codex","task":"auth-api","description":"实现登录 API","depends_on":[]}}
```
（TUI 每 tick 扫描 plan_inbox，与 transcript 互为备份）

工人：codex=后端，kimi=前端，mimo=审查，claude=顾问。不要委派给自己。

## agent-report（工人用；主 Agent 收到回执后处理）

收到 `[协调/…->cursor]` 或 inbox 中的 `【回执·…】` 后必须续派。

## 完整规则

读取：`{coord_s}`

私有 inbox：`../../.agents/{{AGENT_TUI_AGENT}}/memory/inbox.md`（相对 worktree）
"#
    );
    let rules_path = rules_dir.join("agent-tui-orchestrator.mdc");
    fs::write(&rules_path, rules_body).with_context(|| format!("write {}", rules_path.display()))?;

    let agents_stub = worktree.join("AGENTS-agent-tui.md");
    if !agents_stub.exists() {
        let stub = format!(
            "# agent-tui orchestrator\n\n\
             主 Agent 协调规则见 `.cursor/rules/agent-tui-orchestrator.mdc`。\n\
             完整文档：`{coord_s}`\n"
        );
        fs::write(&agents_stub, stub).with_context(|| format!("write {}", agents_stub.display()))?;
    }
    Ok(())
}

#[cfg(windows)]
fn build_cursor_agent_command(args: &[String]) -> Result<portable_pty::CommandBuilder> {
    use portable_pty::CommandBuilder;

    let mut args: Vec<String> = args.to_vec();
    // --trust is only valid with --print / --headless; interactive PTY must not use it.
    if cursor_agent_wants_trust(&args) && !args.iter().any(|a| a == "--trust") {
        args.insert(0, "--trust".into());
    }

    if let Some((node, script)) = resolve_cursor_agent_node_script() {
        let mut cmd = CommandBuilder::new(node);
        cmd.arg(script);
        for arg in &args {
            cmd.arg(arg);
        }
        apply_cursor_agent_env(&mut cmd);
        apply_pty_env(&mut cmd);
        return Ok(cmd);
    }

    let agent_cmd = resolve_agent_cmd().context(
        "Cursor Agent CLI not found. Install Cursor Agent CLI or set AGENT_BIN to agent.cmd",
    )?;
    Ok(wrap_cmd_shim(&agent_cmd, &args))
}

/// Only headless/print cursor agent runs accept `--trust`.
#[cfg(windows)]
fn cursor_agent_wants_trust(args: &[String]) -> bool {
    args.iter().any(|a| a == "--print" || a == "--headless")
}

#[cfg(not(windows))]
fn cursor_agent_wants_trust(_args: &[String]) -> bool {
    false
}

#[cfg(windows)]
fn apply_cursor_agent_env(cmd: &mut portable_pty::CommandBuilder) {
    cmd.env("CURSOR_INVOKED_AS", "agent");
    if std::env::var_os("NODE_COMPILE_CACHE").is_none() {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let cache = Path::new(&local).join("cursor-compile-cache");
            cmd.env("NODE_COMPILE_CACHE", cache.to_string_lossy().to_string());
        }
    }
}

/// Latest `%LOCALAPPDATA%\\cursor-agent\\versions\\*\\node.exe` + `index.js`.
#[cfg(windows)]
fn resolve_cursor_agent_node_script() -> Option<(String, String)> {
    if let Ok(home) = std::env::var("CURSOR_AGENT_HOME") {
        if let Some(pair) = pick_agent_node_script_in(&Path::new(&home).join("versions")) {
            return Some(pair);
        }
    }
    let local = std::env::var("LOCALAPPDATA").ok()?;
    pick_agent_node_script_in(&Path::new(&local).join("cursor-agent").join("versions"))
}

#[cfg(windows)]
fn pick_agent_node_script_in(versions_dir: &Path) -> Option<(String, String)> {
    let mut best: Option<(String, String, std::time::SystemTime)> = None;
    let entries = fs::read_dir(versions_dir).ok()?;
    for entry in entries.flatten() {
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let node = dir.join("node.exe");
        let index = dir.join("index.js");
        if !node.is_file() || !index.is_file() {
            continue;
        }
        let mtime = fs::metadata(&dir).ok()?.modified().ok()?;
        let replace = best
            .as_ref()
            .map(|(_, _, t)| mtime > *t)
            .unwrap_or(true);
        if replace {
            best = Some((
                node.into_os_string().into_string().ok()?,
                index.into_os_string().into_string().ok()?,
                mtime,
            ));
        }
    }
    best.map(|(node, script, _)| (node, script))
}

#[cfg(windows)]
fn resolve_agent_cmd() -> Option<String> {
    if let Ok(explicit) = std::env::var("AGENT_BIN") {
        if Path::new(&explicit).is_file() {
            return Some(explicit);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["agent.cmd", "cursor-agent.cmd"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return candidate.into_os_string().into_string().ok();
                }
            }
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        for name in ["agent.cmd", "cursor-agent.cmd"] {
            let candidate = Path::new(&local).join("cursor-agent").join(name);
            if candidate.is_file() {
                return candidate.into_os_string().into_string().ok();
            }
        }
    }
    None
}

#[cfg(windows)]
fn build_cursor_command(args: &[String]) -> Result<portable_pty::CommandBuilder> {
    let cursor_cmd = resolve_cursor_cmd().context(
        "cursor.cmd not found in PATH — install Cursor or set CURSOR_BIN",
    )?;
    Ok(wrap_cmd_shim(&cursor_cmd, args))
}

#[cfg(windows)]
fn wrap_cmd_shim(program: &str, args: &[String]) -> portable_pty::CommandBuilder {
    use portable_pty::CommandBuilder;

    let mut wrapped = CommandBuilder::new("cmd.exe");
    wrapped.arg("/K");
    wrapped.arg(program);
    for arg in args {
        wrapped.arg(arg);
    }
    apply_pty_env(&mut wrapped);
    wrapped
}

#[cfg(windows)]
fn resolve_cursor_cmd() -> Option<String> {
    if let Ok(explicit) = std::env::var("CURSOR_BIN") {
        if std::path::Path::new(&explicit).is_file() {
            return Some(explicit);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("cursor.cmd");
            if candidate.is_file() {
                return candidate.into_os_string().into_string().ok();
            }
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let candidate = Path::new(&local)
            .join("Programs")
            .join("cursor")
            .join("resources")
            .join("app")
            .join("bin")
            .join("cursor.cmd");
        if candidate.is_file() {
            return candidate.into_os_string().into_string().ok();
        }
    }
    None
}

fn apply_pty_env(cmd: &mut portable_pty::CommandBuilder) {
    cmd.env_remove("NO_COLOR");
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("FORCE_COLOR", "1");
    cmd.env("CLICOLOR_FORCE", "1");
    #[cfg(windows)]
    cmd.env("VTE_VERSION", "6800");
}

#[cfg(windows)]
fn try_build_node_npm_command(program: &str, args: &[String]) -> Option<portable_pty::CommandBuilder> {
    use portable_pty::CommandBuilder;

    let script = npm_cli_script(program)?;
    let mut cmd = CommandBuilder::new("node");
    cmd.arg(script);
    for arg in args {
        cmd.arg(arg);
    }
    Some(cmd)
}

#[cfg(windows)]
fn npm_cli_script(program: &str) -> Option<String> {
    let rel = match program {
        "mimo" => "@mimo-ai/cli/bin/mimo",
        "codex" => "@openai/codex/bin/codex.js",
        _ => return None,
    };
    let appdata = std::env::var_os("APPDATA")?;
    let script = Path::new(&appdata)
        .join("npm")
        .join("node_modules")
        .join(rel);
    if script.is_file() {
        script.into_os_string().into_string().ok()
    } else {
        None
    }
}

#[cfg(windows)]
fn windows_needs_cmd_wrapper(program: &str) -> bool {
    if program.ends_with(".cmd") || program.ends_with(".bat") {
        return true;
    }
    if program.contains('\\') || program.contains('/') {
        return false;
    }
    matches!(program, "npx" | "npm" | "pnpm" | "yarn")
}
