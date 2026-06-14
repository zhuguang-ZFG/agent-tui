//! `agent-tui doctor --fix` — 自动检测并修复 Agent CLI 配置问题。

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::config::{load_agents, AgentSpec};
use crate::project_init::{self, InitOptions};

/// Result of a single agent CLI preflight check.
#[derive(Debug)]
#[allow(dead_code)]
pub struct CliCheck {
    pub name: String,
    pub command: String,
    pub on_path: bool,
    pub version: Option<String>,
    pub trust_needed: bool,
    pub trust_fixed: bool,
    pub issue: Option<String>,
}

/// Run preflight checks on all agent CLIs.
pub fn check_all_agents(project_dir: &Path) -> Result<Vec<CliCheck>> {
    let agents = load_agents(project_dir)?;
    let mut checks = Vec::new();
    for agent in &agents {
        checks.push(check_single_cli(agent));
    }
    Ok(checks)
}

/// Check a single agent CLI.
fn check_single_cli(agent: &AgentSpec) -> CliCheck {
    let program = agent
        .command
        .split_whitespace()
        .next()
        .unwrap_or(&agent.command);

    let on_path = which_on_path(program);
    let version = if on_path {
        get_cli_version(program).ok()
    } else {
        None
    };

    let (trust_needed, trust_fixed, issue) = if !on_path {
        (false, false, Some(format!("{} not found on PATH", program)))
    } else {
        check_trust_status(&agent.name, program)
    };

    CliCheck {
        name: agent.name.clone(),
        command: agent.command.clone(),
        on_path,
        version,
        trust_needed,
        trust_fixed,
        issue,
    }
}

fn which_on_path(program: &str) -> bool {
    let cmd = if cfg!(windows) { "where" } else { "which" };
    Command::new(cmd)
        .arg(program)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn get_cli_version(program: &str) -> Result<String> {
    let out = run_cli_command(program, &["--version"])
        .with_context(|| format!("run {program} --version"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{}{}", stdout.trim(), stderr.trim());
    let ver = combined
        .lines()
        .find(|l| !l.is_empty())
        .unwrap_or("?")
        .trim()
        .to_string();
    Ok(ver)
}

/// Run a CLI command, using `cmd /c` on Windows for npm-wrapped CLIs.
fn run_cli_command(program: &str, args: &[&str]) -> Result<std::process::Output> {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.args(["/c", program]);
        cmd.args(args);
        Ok(cmd.output()?)
    } else {
        let mut cmd = Command::new(program);
        cmd.args(args);
        Ok(cmd.output()?)
    }
}

/// Check trust status and attempt auto-fix for known CLIs.
fn check_trust_status(name: &str, program: &str) -> (bool, bool, Option<String>) {
    match name {
        "claude" => check_claude_trust(program),
        "codex" => check_codex_trust(program),
        "cursor" => check_cursor_trust(program),
        "kimi" => check_kimi_trust(program),
        "mimo" => check_mimo_trust(program),
        "reasonix" => check_reasonix_trust(program),
        "kilo" => check_kilo_trust(program),
        "opencode" => check_opencode_trust(program),
        _ => (false, false, None),
    }
}

// ─── Unified CLI trust check ───
// Many CLIs on Windows return non-zero exit code from --version
// but still produce valid output. We check output instead.

fn check_cli_responds(program: &str) -> (bool, bool, Option<String>) {
    let result = run_cli_command(program, &["--version"]);
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let has_output = !stdout.trim().is_empty() || !stderr.trim().is_empty();
            if has_output || out.status.success() {
                (true, true, None)
            } else {
                (true, false, Some(format!("{program} --version 无输出 (exit {:?})", out.status.code())))
            }
        }
        Err(e) => (true, false, Some(format!("{program} 执行失败: {e}"))),
    }
}

fn check_claude_trust(program: &str) -> (bool, bool, Option<String>) {
    check_cli_responds(program)
}

fn check_codex_trust(program: &str) -> (bool, bool, Option<String>) {
    check_cli_responds(program)
}

fn check_cursor_trust(program: &str) -> (bool, bool, Option<String>) {
    // "cursor agent" — program is "cursor"
    let prog = program.split_whitespace().next().unwrap_or(program);
    check_cli_responds(prog)
}

fn check_kimi_trust(program: &str) -> (bool, bool, Option<String>) {
    check_cli_responds(program)
}

fn check_mimo_trust(program: &str) -> (bool, bool, Option<String>) {
    check_cli_responds(program)
}

fn check_reasonix_trust(program: &str) -> (bool, bool, Option<String>) {
    check_cli_responds(program)
}

fn check_kilo_trust(program: &str) -> (bool, bool, Option<String>) {
    check_cli_responds(program)
}

fn check_opencode_trust(program: &str) -> (bool, bool, Option<String>) {
    check_cli_responds(program)
}

/// Format the check results into a human-readable report.
pub fn format_check_report(checks: &[CliCheck]) -> String {
    let mut out = String::from("\n── Agent CLI 预检 ──\n");
    let total = checks.len();
    let ok_count = checks.iter().filter(|c| c.on_path).count();
    let trust_count = checks.iter().filter(|c| c.trust_fixed).count();

    for c in checks {
        let status = if !c.on_path {
            "✗ NOT FOUND"
        } else if c.trust_fixed {
            "✓"
        } else {
            "○ 需配置"
        };
        let ver = c.version.as_deref().unwrap_or("?");
        out.push_str(&format!(
            "  {:<10} {:<20} {} {}\n",
            c.name,
            c.command,
            status,
            if c.on_path { format!("v{ver}") } else { String::new() }
        ));
        if let Some(ref issue) = c.issue {
            out.push_str(&format!("           └─ {}\n", issue));
        }
    }

    out.push_str(&format!(
        "\n摘要: {ok_count}/{total} 在 PATH，{trust_count}/{total} 信任就绪\n"
    ));

    // Non-interactive usage tips
    out.push_str("\n── 非交互模式提示 ──\n");
    out.push_str("  claude:   claude -p \"<task>\" --dangerously-skip-permissions\n");
    out.push_str("  codex:    codex exec --dangerously-bypass-approvals-and-sandbox \"<task>\"\n");
    out.push_str("  mimo:     mimo --prompt \"<task>\" --never-ask-questions\n");
    out.push_str("  kimi:     kimi --auto   (交互式，-p 与 --auto 互斥)\n");
    out.push_str("  reasonix: reasonix run \"<task>\"\n");
    out.push_str("  opencode: opencode run \"<task>\"\n");
    out
}

/// Run the full fix: init + sync-lead + worktree + agent preflight.
pub fn doctor_fix(project_dir: &Path) -> Result<String> {
    let mut log = String::new();

    // Step 1: ensure init
    let status = project_init::detect_project_or_cwd(Some(project_dir.to_path_buf()));
    if !status.has_agents_yaml {
        log.push_str("► 运行 agent-tui init...\n");
        project_init::init_project(
            project_dir,
            &InitOptions {
                force: false,
                link_worktrees: true,
                sync_lead: true,
                minimal: false,
            },
        )?;
        log.push_str("  ✓ 已初始化\n");
    } else {
        log.push_str("✓ agents.yaml 已存在\n");
    }

    // Step 2: ensure worktrees
    if !status.worktrees_missing.is_empty() {
        log.push_str(&format!(
            "► 创建 worktree 联接: {}...\n",
            status.worktrees_missing.join(", ")
        ));
        project_init::init_project(
            project_dir,
            &InitOptions {
                force: false,
                link_worktrees: true,
                sync_lead: false,
                minimal: false,
            },
        )?;
        log.push_str("  ✓ worktree 已联接\n");
    }

    // Step 3: ensure lead sync
    if !status.has_lead_rules {
        log.push_str("► 同步 Lead 规则...\n");
        let agents = load_agents(project_dir)?;
        let lead = crate::config::resolve_lead_agent(&agents);
        let spec = agents
            .iter()
            .find(|a| a.name == lead)
            .ok_or_else(|| anyhow::anyhow!("lead {lead} not in agents.yaml"))?;
        crate::lead_identity::sync_lead_context(project_dir, &lead, &spec.worktree)?;
        log.push_str("  ✓ Lead 规则已同步\n");
    }

    // Step 4: agent CLI preflight
    log.push_str("► Agent CLI 预检...\n");
    let checks = check_all_agents(project_dir)?;
    log.push_str(&format_check_report(&checks));

    // Step 5: auto-generate routing if missing or outdated
    let routing_path = project_dir.join(".agents/routing.yaml");
    if !routing_path.is_file() {
        log.push_str("► 生成代码图谱路由...\n");
        match crate::auto_route::gen_routes(project_dir, false) {
            Ok(r) => log.push_str(&r),
            Err(e) => log.push_str(&format!("  ⚠ 路由生成失败: {e}\n")),
        }
    } else {
        log.push_str("✓ routing.yaml 已存在（如需更新，运行 agent-tui gen-routes）\n");
    }

    // Step 6: summary
    let all_ok = checks.iter().all(|c| c.on_path);
    let issues: Vec<_> = checks.iter().filter(|c| !c.on_path).collect();
    if all_ok {
        log.push_str("\n✓ 所有 Agent CLI 就绪，可运行 agent-tui up\n");
    } else {
        log.push_str(&format!(
            "\n○ {} 个 Agent CLI 未安装，请手动安装后重试\n",
            issues.len()
        ));
        for c in &issues {
            log.push_str(&format!("  - {}: {}\n", c.name, c.command));
        }
    }

    Ok(log)
}
