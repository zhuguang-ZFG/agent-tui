//! Human-facing cheat sheet — you only need a few commands.

use std::path::Path;

use crate::config;

pub fn cheat_sheet(project_dir: Option<&Path>) -> String {
    let project_hint = project_dir
        .map(|p| format!(" --project-dir {}", p.display()))
        .unwrap_or_else(|| " --project-dir <项目根>".into());

    let agents = project_dir.and_then(|p| config::load_agents(p).ok());
    let lead = agents
        .as_ref()
        .map(|a| config::resolve_lead_agent(a))
        .unwrap_or_else(|| "cursor".into());
    let agent_count = agents.as_ref().map(|a| a.len()).unwrap_or(0);
    let layout_desc = match agent_count {
        0 => "（未检测到 Agent）".into(),
        1 => "单格全屏".into(),
        2 => "左右两栏".into(),
        3 => "三列并排".into(),
        4 => "2×2 四宫格".into(),
        n => format!("上 3 + 下 {} 网格", n - 3),
    };

    let spawn_mode = if agent_count > 4 {
        format!("Lead 优先启动，其余 {} 个按需", agent_count - 1)
    } else {
        format!("{agent_count} 个 Agent 同时启动")
    };

    format!(
        r#"agent-tui 速查（只记 1 条）

  ★ agent-tui{project_hint}
    或 agent-tui up{project_hint}
    → 未 init 会自动初始化，{agent_count} Agent 自适应布局（{layout_desc}）
    → {spawn_mode}（--eager 全启动）

  单格全屏：agent-tui --solo  |  F2 切换网格/全屏

  忘了操作？TUI 内 Ctrl+G 速查  |  Ctrl+N 当前进度  |  Ctrl+I 留言板 !doctor 体检

  省资源：同项目重启 TUI 不重复灌 Lead briefing；记忆 FTS 仅在文件变更时重建

────────────────────────────────────────
偶尔才用（不必背）

  agent-tui init{project_hint}   强制重新脚手架（一般不用）
  agent-tui doctor{project_hint}  仅体检、不启动 TUI
  agent-tui guide{project_hint}   打印本页

  ※ Lead 格子里的 `agent` 是 Cursor CLI，不是入口；入口永远是 agent-tui。

────────────────────────────────────────
在 TUI 里完成全流程（Lead = {lead}）

  · Ctrl+1 聚焦 Lead，用自然语言说需求
  · Ctrl+I：!任务 实现某某功能
  · Ctrl+N：当前阶段 + 下一步（等同 next）
  · Ctrl+G：速查（等同 guide）
  · Ctrl+T：任务看板按批次分组（Space 折叠），错配显示 →建议 @worker
  · 路由表：.agents/routing.yaml + STRENGTHS.md
  · 专家：.agents/specialists/*.md（委派时自动注入专家简报）
  · PROJECT_MAP：!map / agent-tui map；merge-ready 后自动刷新（24h）
  · 危险操作（!pr / !merge-all 等）需 Y 确认

TUI 自动完成（无需手动）：

  派活 → 工人执行 → 回执 → 续派
  → 逐 task review → 批次审查 → 【merge-ready】
  → smoke →（可选）自动开 PR / merge / 轮询 merged

────────────────────────────────────────
留言板 Ctrl+I — 运维与交付（等同子命令）

  !任务 描述需求          → 交给 Lead 拆解
  !委派 @codex task 说明   → 手动派活
  !回执 task 说明          → 手动回执
  !pr                     → pr-create（开 PR）
  !pr-merge               → 合并当前 PR
  !pr-status              → 查询 PR 状态
  !merge / !merge-all     → 合并 agent 分支
  !review [--force]       → 批次审查 / 重审
  !clean                  → 清理 verify-loop 残留
  !reset-batch            → 新 sprint 重置指纹
  !sync-lead              → 刷新 Lead 规则
  !doctor                 → 项目体检
  !doctor --fix            → 体检 + 自动修复
  !map                    → 生成 PROJECT_MAP.md
  !next / !guide          → 打开进度 / 速查面板
  @mimo 消息               → 定向通知某 Agent

────────────────────────────────────────
仍需 CLI 或改文件（暂不在 TUI）

  agent-tui init          首次初始化项目
  agents.yaml enabled:false  关闭某 Agent（改后重启 TUI）
  环境变量 AGENT_TUI_AUTO_PR / AGENT_TUI_AUTO_MERGE  启动 TUI 前设置

────────────────────────────────────────
偶尔才用的 CLI（开发/CI）

  agent-tui doctor --fix{project_hint}  一键预检 + 自动修复
  agent-tui gen-routes{project_hint}    扫描代码结构自动生成路由
  agent-tui plan <task>{project_hint}   智能拆解大任务为子任务计划
  agent-tui memory{project_hint}        跨 Agent 记忆总览
  agent-tui map{project_hint}           生成 PROJECT_MAP.md
  agent-tui verify-loop{project_hint}   闭环自检

────────────────────────────────────────
快捷键（TUI 内）

  Ctrl+1~8  聚焦 Agent    Ctrl+I  留言板
  F5        重启失败格子    F2  网格/单人
  Ctrl+T    任务看板（按批次分组，Space 折叠）
  Ctrl+E    事件时间线
  Ctrl+G    速查            Ctrl+N  当前进度
  Ctrl+Q    退出

环境变量（启动 TUI 前设置，可选全自动交付）：
  AGENT_TUI_AUTO_PR=1      合并就绪后自动 gh pr create
  AGENT_TUI_AUTO_MERGE=1   CI 通过后自动 merge PR
  AGENT_TUI_POLL_PR=1      轮询 PR 合并状态（默认开）
  AGENT_TUI_FTS_ALWAYS=1   每次启动强制重建记忆 FTS 索引

详细流程：项目内 .agents/LEAD.md 或 agent-tui 仓库 docs/WORKFLOW.md
"#
    )
}
