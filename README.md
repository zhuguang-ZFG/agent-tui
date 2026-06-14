# agent-tui

Windows 原生多 Agent TUI：自适应网格嵌入最多 8 个 Agent PTY（`ratatui` + `vt100` + ConPTY），自动解析 **agent-plan** / **agent-report**，驱动委派、回执、DAG、死信重试、Lead 续派与 Web 观测。

> **按需启动**：Agent 数 > 4 时仅启动 Lead，其余 Agent 收到任务时自动启动，内存占用降低 80%。

配合 [agents-complete / solid-guacamole](https://github.com/zhuguang-ZFG/solid-guacamole)（Git Worktree + `agents.yaml`）使用。

## 文档

| 文档 | 说明 |
|------|------|
| **[docs/WORKFLOW.md](docs/WORKFLOW.md)** | **全流程指南**（架构、生命周期、协议、CLI、环境变量、排障） |
| [docs/COORDINATION-template.md](docs/COORDINATION-template.md) | 复制到项目 `.agents/COORDINATION.md` 的 Agent 规则模板 |

## 快速开始

### 编译

```powershell
cd agent-tui
cargo build --release
# 可选：输出到 bin/
Copy-Item -Force target\release\agent-tui.exe .\bin\
```

### 启动

```powershell
# 只记这一条（任意项目目录；首次会自动 init）
agent-tui
# 或
agent-tui up --project-dir D:\your-project

# 可选：强制全启动（默认 > 4 Agent 时按需启动）
agent-tui --eager
agent-tui --max-agents 4           # 只启动前 4 个 Agent

# 可选：仅体检
agent-tui doctor
agent-tui doctor --fix             # 体检 + 自动修复（npm link / 信任配置）
```

### 闭环验证

```powershell
agent-tui verify-loop --project-dir D:\your-project
```

### Web 观测

```powershell
agent-tui serve --project-dir D:\your-project
# http://127.0.0.1:8787/
```

## 核心能力

- **按需启动（Lazy Spawn）**：> 4 Agent 时仅启动 Lead，委派/聚焦时自动启动目标 Agent
- **自动派发**：扫描 Lead PTY 的 `agent-plan` + `plan_inbox.jsonl` 双通道
- **自动回执**：扫描工人 PTY 的 `agent-report` → 通知 Lead
- **Lead 续派**：回执后 transcript 尾部重扫 + 超时 nudge
- **DAG**：`depends_on` 前置任务完成后延迟派发
- **死信重试**：failed/blocked → 指数退避 + 工人轮换
- **Relay**：events / mailbox → PTY 注入 `[协调/…]`
- **OOM 自动重启**：检测 `error.OutOfMemory` / `Effect.tryPromise` 等 FATAL 模式
- **长期记忆**：各 Agent `memory/` + FTS5 全文检索（`Ctrl+T`）
- **Observer**：只读 Web 面板 + SSE 实时推送
- **Doctor --fix**：一键诊断修复（npm link、CLI 信任配置、MCP 检查）
- **Gen-routes**：基于代码图谱自动生成 `routing.yaml` 路由规则
- **Plan**：智能任务分解（feature / bugfix / refactor 模板）

## 快捷键

| 按键 | 功能 |
|------|------|
| `Ctrl+1`~`Ctrl+8` | 聚焦面板（按需启动） |
| `Ctrl+Tab` | 切换面板 |
| `Ctrl+I` | 留言板 |
| `Ctrl+T` | 任务看板 / 记忆搜索 |
| `Ctrl+E` | 协调事件时间线 |
| `F2` | 网格 / 单人全屏 |
| `F5` | 重启当前失败/空格子 |
| `Ctrl+Q` | 退出 |

## CLI 子命令

```powershell
agent-tui                    # 默认：自动 init + 自适应网格 TUI
agent-tui up                 # 同上
agent-tui init|doctor|guide|next|map|sync-lead
agent-tui notify|broadcast|delegate|report ...
agent-tui merge [--all]|pr-create|pr-status|pr-merge
agent-tui plan-submit|plan-dry-run|review|evolve|reset-batch|clean-verify
agent-tui tasks|events|memory-search|memory-reindex
agent-tui gen-routes         # 自动生成 routing.yaml
agent-tui plan <task>        # 智能任务分解
agent-tui memory <agent>     # 查看 Agent 记忆
agent-tui verify-loop|verify-live|serve
```

留言板 `!` 命令：

```powershell
!任务 描述需求                # 交给 Lead 拆解
!委派 @codex task 说明        # 手动派活
!doctor --fix                 # 体检 + 自动修复
!pr / !pr-merge / !pr-status  # PR 管理
!merge / !merge-all           # 合并 agent 分支
!review [--force]             # 批次审查
!map                          # 刷新 PROJECT_MAP
!gen-routes                   # 重新生成路由规则
```

详见 [docs/WORKFLOW.md §7–§8](docs/WORKFLOW.md#7-cli-命令一览)。

## 环境变量

| 变量 | 说明 |
|------|------|
| `NODE_OPTIONS` | 自动设置 `--max-old-space-size=512`（限制 V8 堆 512MB） |
| `AGENT_TUI` | 值为 `1` 表示运行在 agent-tui 内 |
| `AGENT_TUI_AGENT` | 当前 Agent 名称 |
| `AGENT_TUI_LEAD` | Lead Agent 名称 |
| `AGENT_TUI_ORCHESTRATOR` | Lead Agent 额外设为 `1` |

## 诊断

```powershell
agent-tui --check --project-dir D:\your-project
agent-tui doctor --fix --project-dir D:\your-project
cargo run --release --example pty_probe
```

## License

MIT
