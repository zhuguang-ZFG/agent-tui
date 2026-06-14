mod batch_reset;
mod confirm_panel;
mod specialists;
mod project_map;
mod batch_group;
mod routing;
mod batch_review;
mod agent_memory;
mod agent_strengths;
mod app;
mod claims;
mod coord_dedupe;
mod config;
mod conpty;
mod dead_letter;
mod delegation;
mod delegation_stats;
mod event_timeline;
mod events_ui;
mod guide;
mod help_panel;
mod ops;
mod health;
mod inbox_ui;
mod lead_followup;
mod lead_identity;
mod lead_watch;
mod mailbox;
mod mailbox_relay;
mod memory_fts;
mod merge_ready;
mod auto_pr;
mod merge;
mod post_merge_smoke;
mod meta;
mod plan_inbox;
mod pr_create;
mod pr_lifecycle;
mod project_init;
mod observer;
mod pane;
mod relay;
mod report_gate;
mod report_watch;
mod review_gate;
mod task_board;
mod task_dag;
mod task_state;
mod tasks_ui;
mod terminal;
mod ui;
mod verify_live;
mod verify_cleanup;
mod verify_loop;
mod workflow_phase;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::event::Event;
use ratatui::DefaultTerminal;

#[derive(Parser, Debug)]
#[command(
    name = "agent-tui",
    about = "Windows 原生多 Agent 控制台（vt100）",
    after_help = "只记一条：在项目目录运行 agent-tui 或 agent-tui up（自动 init + 进 TUI）"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Project root containing .agents/agents.yaml
    #[arg(long)]
    project_dir: Option<PathBuf>,

    /// Spawn only the first N enabled agents (default: all, max 8)
    #[arg(long)]
    max_agents: Option<usize>,

    /// Start focused on this agent (e.g. claude, mimo)
    #[arg(long)]
    agent: Option<String>,

    /// Start solo fullscreen on one agent (default: grid — all enabled agents visible)
    #[arg(long)]
    solo: bool,

    /// Load config and print agents, then exit (no TUI)
    #[arg(long)]
    check: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 一条命令：未初始化则自动 init，然后启动 TUI（推荐）
    Up {
        #[arg(long)]
        project_dir: Option<PathBuf>,
        #[arg(long)]
        max_agents: Option<usize>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        solo: bool,
    },
    /// Agent 间发消息（写入 inbox + events，TUI 运行时会自动注入 PTY）
    Notify {
        /// 目标 Agent
        agent: String,
        /// 消息正文
        message: String,
        /// 发送方（Agent 名或 user）
        #[arg(long, default_value = "user")]
        from: String,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 广播给所有 Agent
    Broadcast {
        message: String,
        #[arg(long, default_value = "user")]
        from: String,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 主 Agent 委派任务（认领 + 通知工人 + Relay）
    Delegate {
        worker: String,
        task: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 工人向主 Agent 回执（加 --status 则更新 task_state）
    Report {
        message: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "user")]
        from: String,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 调试：从文本解析 agent-plan（加 --execute 则真实委派）
    PlanDryRun {
        /// 含 agent-plan 的样本文本，或 @file 路径
        text: String,
        #[arg(long)]
        project_dir: Option<PathBuf>,
        #[arg(long)]
        execute: bool,
    },
    /// 无 TUI 闭环验证：解析 + 委派 + 回执 + events.jsonl 检查
    VerifyLoop {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// Live PTY 验证：注入协调文本 + transcript 解析器
    VerifyLive {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 任务看板：活跃认领 / 已完成 / 等待依赖
    Tasks {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 搜索 Agent 长期记忆（FTS5）
    MemorySearch {
        /// 搜索关键词
        query: String,
        #[arg(long, default_value_t = 15)]
        limit: usize,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 重建记忆 FTS 索引
    MemoryReindex {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 打印协调事件时间线（与 TUI Ctrl+E 相同数据源）
    Events {
        #[arg(long)]
        project_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// 向 plan_inbox.jsonl 提交计划并立即派发
    PlanSubmit {
        /// agent-plan JSON 数组或含 agent-plan 的文本，或 @file
        text: String,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 只读 Web 观测面板（mailbox / task_state / 时间线）
    Serve {
        #[arg(long, default_value_t = 8787)]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 刷新 Lead 规则（LEAD.md + Cursor orchestrator.mdc + AGENTS 指针）
    SyncLead {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 生成或刷新 `.agents/PROJECT_MAP.md`（项目结构速览）
    Map {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 在当前/指定项目脚手架 `.agents/`（agents.yaml + COORDINATION + sync-lead）
    Init {
        #[arg(long)]
        project_dir: Option<PathBuf>,
        /// 覆盖已有 agents.yaml / COORDINATION.md
        #[arg(long)]
        force: bool,
        /// 不创建 worktree 联接（多 Agent 共用项目根 cwd）
        #[arg(long)]
        no_link_worktrees: bool,
        /// 跳过 sync-lead
        #[arg(long)]
        no_sync_lead: bool,
        /// 最小名册：cursor + codex
        #[arg(long)]
        minimal: bool,
    },
    /// 检测项目是否已配置 agent-tui（无需 agents.yaml 也可运行）
    Doctor {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 打印速查表（只记 3 条命令）
    Guide {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 当前阶段 + 一条建议命令
    Next {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// merge-ready 后创建 GitHub PR（包装 gh pr create）
    PrCreate {
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, default_value = "main")]
        base: String,
        #[arg(long)]
        draft: bool,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 汇总委派历史并更新 STRENGTHS.md（运行时进化）
    Evolve {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 计划完成后委派批次代码审查（reviewer）
    Review {
        /// 仅审查 task 前缀匹配的批次（同 AGENT_TUI_MERGE_BATCH）
        #[arg(long)]
        batch: Option<String>,
        /// 批次审查 failed 后清除 dispatched 指纹并重新委派
        #[arg(long)]
        force: bool,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 合并多 agent worktree 分支（包装 .agents/merge.sh 或 --all 非交互）
    Merge {
        /// 非交互：自动合并所有 agent 分支到 merge-* 分支
        #[arg(long)]
        all: bool,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 查询当前分支 PR 状态（gh pr view）
    PrStatus {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 合并当前分支 PR（gh pr merge --auto）
    PrMerge {
        #[arg(long)]
        squash: bool,
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 清理 verify-loop 残留在项目中的测试任务
    CleanVerify {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// 重置交付批次指纹（新 sprint 前，不清 task_state）
    ResetBatch {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(cmd) = cli.command {
        return run_command(cmd);
    }

    run_tui(
        cli.project_dir,
        cli.max_agents,
        cli.agent.as_deref(),
        cli.solo,
        cli.check,
    )
}

fn run_tui(
    project_dir: Option<PathBuf>,
    max_agents: Option<usize>,
    initial_agent: Option<&str>,
    solo_on_start: bool,
    check_only: bool,
) -> Result<()> {
    let project_dir = project_init::ensure_project_ready(project_dir)?;

    if check_only {
        let agents = config::load_agents(&project_dir)?;
        println!("项目：{}", project_dir.display());
        println!("Agent 数量：{}", agents.len());
        for a in &agents {
            println!(
                "  {} 角色={} worktree={}",
                a.name,
                a.role,
                a.worktree.display()
            );
        }
        return Ok(());
    }

    crossterm::style::force_color_output(true);
    #[cfg(windows)]
    {
        let _ = crossterm::ansi_support::supports_ansi();
    }

    install_panic_hook();
    let mut guard = terminal::TerminalGuard::enter();
    let mut terminal = ratatui::init();
    let result = run(
        &mut terminal,
        &mut guard,
        project_dir.clone(),
        max_agents,
        initial_agent,
        solo_on_start,
    );
    terminal::restore_host_terminal();
    guard.disarm();
    if let Err(ref e) = result {
        terminal::log_message(&project_dir, "error", &format!("TUI 退出: {e:#}"));
        eprintln!("agent-tui 异常退出: {e:#}");
        eprintln!("详情见 {}", project_dir.join(".agents/agent-tui.log").display());
    }
    result
}

fn run(
    terminal: &mut DefaultTerminal,
    _guard: &mut terminal::TerminalGuard,
    project_dir: PathBuf,
    max_agents: Option<usize>,
    initial_agent: Option<&str>,
    solo_on_start: bool,
) -> Result<()> {
    let mut app = app::App::new(project_dir.clone())?;
    if let Some(max) = max_agents {
        app.agents.truncate(max.clamp(1, 8));
        app.panes = (0..app.agents.len().clamp(1, 8)).map(|_| None).collect();
        app.init_tracking_vectors();
    }
    if let Some(name) = initial_agent {
        app.focus_agent_by_name(name);
    } else if solo_on_start {
        app.show_solo();
    }

    let _observer_guard = if observer::observer_enabled() {
        observer::spawn_background(project_dir.clone())?;
        true
    } else {
        false
    };
    let _ = _observer_guard;

    let size = terminal.size()?;
    app.queue_spawn_all(size.height, size.width);
    app.dirty = true;

    loop {
        if app.should_quit {
            terminal::log_message(&app.project_dir, "info", "用户退出 (Ctrl+Q)");
            let _ = terminal.clear();
            break;
        }

        app.tick();

        if app.needs_redraw() {
            match terminal.draw(|f| ui::draw(f, &mut app)) {
                Ok(_) => app.mark_rendered(),
                Err(e) => {
                    terminal::log_message(
                        &app.project_dir,
                        "error",
                        &format!("draw 失败: {e:#}"),
                    );
                }
            }
        }

        match app::poll_event(Duration::from_millis(50)) {
            Ok(Some(event)) => {
                if let Event::Resize(cols, rows) = event {
                    if rows >= 8 && cols >= 40 {
                        app.resize_terminal(rows, cols);
                    }
                } else {
                    app.handle_event(event);
                }
            }
            Ok(None) => {}
            Err(e) => {
                terminal::log_message(
                    &app.project_dir,
                    "warn",
                    &format!("poll 事件失败（继续运行）: {e:#}"),
                );
                let _ = crossterm::terminal::enable_raw_mode();
            }
        }
    }

    Ok(())
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        terminal::restore_host_terminal();
        default(info);
    }));
}

fn run_command(cmd: Commands) -> Result<()> {
    match cmd {
        Commands::Up {
            project_dir,
            max_agents,
            agent,
            solo,
        } => return run_tui(project_dir, max_agents, agent.as_deref(), solo, false),
        Commands::Notify {
            agent,
            message,
            from,
            project_dir,
        } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            meta::notify_agent_from(&project_dir, &agent, &message, &from)?;
            println!("已通知 {agent}（from={from}）");
        }
        Commands::Broadcast {
            message,
            from,
            project_dir,
        } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            meta::broadcast_message(&project_dir, &message, &from)?;
            println!("已广播（from={from}）");
        }
        Commands::Delegate {
            worker,
            task,
            description,
            from,
            project_dir,
        } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let agents = config::load_agents(&project_dir)?;
            let lead = from.unwrap_or_else(|| config::resolve_lead_agent(&agents));
            delegation::delegate_task(
                &project_dir,
                &lead,
                &worker,
                &task,
                description.as_deref().unwrap_or(""),
            )?;
            println!("已委派 {worker} 任务「{task}」（主 Agent: {lead}）");
        }
        Commands::Report {
            message,
            task,
            status,
            to,
            from,
            project_dir,
        } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let agents = config::load_agents(&project_dir)?;
            let lead = to.unwrap_or_else(|| config::resolve_lead_agent(&agents));
            if let (Some(t), Some(s)) = (task.as_deref(), status.as_deref()) {
                delegation::report_task_auto(
                    &project_dir,
                    &from,
                    &lead,
                    t,
                    s,
                    &message,
                )?;
                println!("已回执 {s} 给 {lead}（from={from}, task={t}）");
            } else {
                delegation::report_task(
                    &project_dir,
                    &from,
                    &lead,
                    task.as_deref(),
                    &message,
                )?;
                println!("已回执给 {lead}（from={from}）");
            }
        }
        Commands::PlanDryRun {
            text,
            project_dir,
            execute,
        } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let agents = config::load_agents(&project_dir)?;
            let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
            let lead = config::resolve_lead_agent(&agents);
            let text = if let Some(path) = text.strip_prefix('@') {
                std::fs::read_to_string(path.trim())
                    .with_context(|| format!("read plan text file {path}"))?
            } else {
                text
            };
            let mut seen = std::collections::HashSet::new();
            let items = lead_watch::plans_from_text(&text, &names, &mut seen, Some(&project_dir));
            if items.is_empty() {
                println!("未解析到可派发的 agent-plan");
                std::process::exit(1);
            }
            for item in &items {
                println!(
                    "  {} → {} ({})",
                    lead,
                    item.worker,
                    item.task
                );
            }
            if execute {
                let n = lead_watch::dispatch_plan_items(&project_dir, &lead, items, "cli");
                println!("已委派 {n} 个子任务");
            } else {
                println!("（dry-run，加 --execute 真实委派）");
            }
        }
        Commands::VerifyLoop { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            verify_loop::run_all(&project_dir)?;
        }
        Commands::VerifyLive { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            verify_live::run_live(&project_dir)?;
        }
        Commands::Tasks { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            println!("{}", task_board::format_task_board(&project_dir)?);
        }
        Commands::MemorySearch {
            query,
            limit,
            project_dir,
        } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let hits = memory_fts::search(&project_dir, &query, limit)?;
            if hits.is_empty() {
                println!("（无匹配）");
            } else {
                for line in memory_fts::format_hits(&hits) {
                    println!("{line}");
                }
            }
        }
        Commands::MemoryReindex { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let n = memory_fts::reindex_all(&project_dir)?;
            println!("已索引 {n} 个记忆文件 → {}", project_dir.join(".agents/shared/memory_fts.db").display());
        }
        Commands::Events { project_dir, limit } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            for line in event_timeline::build_timeline_lines(&project_dir, limit) {
                println!("{line}");
            }
        }
        Commands::PlanSubmit { text, project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let agents = config::load_agents(&project_dir)?;
            let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
            let lead = config::resolve_lead_agent(&agents);
            let text = if let Some(path) = text.strip_prefix('@') {
                std::fs::read_to_string(path.trim())
                    .with_context(|| format!("read plan file {path}"))?
            } else {
                text
            };
            let items = if text.trim_start().starts_with('[') {
                lead_watch::parse_plan_items(&text, &names)
            } else {
                let mut seen = std::collections::HashSet::new();
                lead_watch::plans_from_text(&text, &names, &mut seen, Some(&project_dir))
            };
            if items.is_empty() {
                println!("未解析到 plan 条目");
                std::process::exit(1);
            }
            let n = plan_inbox::submit_items(&project_dir, &lead, items)?;
            println!("plan_inbox 已提交并派发 {n} 个子任务");
        }
        Commands::Serve {
            port,
            bind,
            project_dir,
        } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let addr = format!("{bind}:{port}");
            observer::serve_blocking(project_dir, &addr)?;
        }
        Commands::SyncLead { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let agents = config::load_agents(&project_dir)?;
            let lead = config::resolve_lead_agent(&agents);
            let spec = agents
                .iter()
                .find(|a| a.name.eq_ignore_ascii_case(&lead))
                .context("lead agent not in agents.yaml")?;
            lead_identity::sync_lead_context(&project_dir, &lead, &spec.worktree)?;
            let _ = agent_memory::refresh_lead_identity(&project_dir, &lead);
            println!(
                "已同步 Lead 身份规则 → {}",
                spec.worktree.join(".cursor/rules/agent-tui-orchestrator.mdc").display()
            );
            println!(
                "Playbook → {}",
                project_dir.join(".agents/LEAD.md").display()
            );
        }
        Commands::Map { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let msg = project_map::generate(&project_dir)?;
            println!("{msg}");
        }
        Commands::Init {
            project_dir,
            force,
            no_link_worktrees,
            no_sync_lead,
            minimal,
        } => {
            let dir = project_dir
                .map(|p| p.canonicalize().unwrap_or(p))
                .or_else(|| std::env::current_dir().ok())
                .context("need --project-dir or cwd")?;
            let outcome = project_init::init_project(
                &dir,
                &project_init::InitOptions {
                    force,
                    link_worktrees: !no_link_worktrees,
                    sync_lead: !no_sync_lead,
                    minimal,
                },
            )?;
            print!("{}", project_init::format_init_summary(&outcome));
        }
        Commands::Doctor { project_dir } => {
            let status = project_init::detect_project_or_cwd(project_dir);
            print!("{}", project_init::format_doctor_report(&status));
            if !status.ready_for_tui {
                std::process::exit(1);
            }
        }
        Commands::Guide { project_dir } => {
            let dir = project_init::detect_project_or_cwd(project_dir).project_dir;
            print!("{}", guide::cheat_sheet(Some(&dir)));
        }
        Commands::Next { project_dir } => {
            let dir = project_init::detect_project_or_cwd(project_dir).project_dir;
            let snap = workflow_phase::evaluate(&dir);
            print!("{}", workflow_phase::format_next_report(&snap));
        }
        Commands::PrCreate {
            title,
            body,
            base,
            draft,
            project_dir,
        } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let outcome = pr_create::create_pr(
                &project_dir,
                title.as_deref(),
                body.as_deref(),
                Some(base.as_str()),
                draft,
            )?;
            println!("{}", outcome.message);
            if let Some(url) = outcome.url {
                println!("{url}");
            }
            if !outcome.created {
                std::process::exit(1);
            }
        }
        Commands::Evolve { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let n = delegation_stats::evolve_project(&project_dir)?;
            println!("已更新 .agents/STRENGTHS.md 历史表现（{n} 条委派记录）");
        }
        Commands::Review { batch, force, project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            if force {
                batch_review::reset_dispatched(&project_dir)?;
            }
            let task_id =
                batch_review::dispatch_review_for_project(&project_dir, batch.as_deref())?;
            println!("已委派批次代码审查：{task_id}");
        }
        Commands::Merge {
            all,
            base,
            project_dir,
        } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let outcome = merge::run_merge(&project_dir, all, base.as_deref())?;
            println!("{}", outcome.message);
            if let Some(branch) = outcome.merge_branch {
                println!("合并分支: {branch}");
            }
            if !outcome.merged_agents.is_empty() {
                println!("已合并: {}", outcome.merged_agents.join(", "));
            }
        }
        Commands::PrStatus { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let report = pr_lifecycle::query_pr_status(&project_dir)?;
            println!("分支: {}", report.branch);
            println!("状态: {:?}", report.state);
            println!("{}", report.message);
            if let Some(url) = report.url {
                println!("URL: {url}");
            }
        }
        Commands::PrMerge { squash, project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let msg = pr_lifecycle::merge_pr(&project_dir, squash)?;
            println!("{msg}");
            println!("提示: TUI 将轮询合并状态并自动派 post-github-merge 验证（AGENT_TUI_POLL_PR=1）");
        }
        Commands::CleanVerify { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let report = verify_cleanup::cleanup_verify_artifacts(&project_dir)?;
            println!("{}", verify_cleanup::format_cleanup_report(&report));
        }
        Commands::ResetBatch { project_dir } => {
            let project_dir = config::resolve_project_dir(project_dir)?;
            let report = batch_reset::reset_batch_fingerprints(&project_dir)?;
            println!("{}", batch_reset::format_reset_report(&report));
            if !report.removed_files.is_empty() {
                for f in &report.removed_files {
                    println!("  - {f}");
                }
            }
        }
    }
    Ok(())
}
