# agent-tui

Windows 原生多 Agent TUI：在 2×2 网格嵌入最多 8 个 Agent PTY（`ratatui` + `vt100` + ConPTY），自动解析 **agent-plan** / **agent-report**，驱动委派、回执、DAG、死信重试、Lead 续派与 Web 观测。

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

# 可选：仅体检
agent-tui doctor
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

- **自动派发**：扫描 Lead PTY 的 `agent-plan` + `plan_inbox.jsonl` 双通道
- **自动回执**：扫描工人 PTY 的 `agent-report` → 通知 Lead
- **Lead 续派**：回执后 transcript 尾部重扫 + 超时 nudge
- **DAG**：`depends_on` 前置任务完成后延迟派发
- **死信重试**：failed/blocked → 指数退避 + 工人轮换
- **Relay**：events / mailbox → PTY 注入 `[协调/…]`
- **长期记忆**：各 Agent `memory/` + FTS5 全文检索（`Ctrl+T`）
- **Observer**：只读 Web 面板 + SSE 实时推送

## 快捷键

| 按键 | 功能 |
|------|------|
| `Ctrl+1`~`Ctrl+8` | 聚焦面板 |
| `Ctrl+Tab` | 切换面板 |
| `Ctrl+I` | 留言板 |
| `Ctrl+T` | 任务看板 / 记忆搜索 |
| `Ctrl+E` | 协调事件时间线 |
| `F2` | 四宫格 / 单人全屏 |
| `F5` | 重启当前失败/空格子 |
| `Ctrl+Q` | 退出 |

## CLI 子命令

```powershell
agent-tui                    # 默认：自动 init + 四宫格 TUI
agent-tui up                 # 同上
agent-tui init|doctor|guide|next|map|sync-lead
agent-tui notify|broadcast|delegate|report ...
agent-tui merge [--all]|pr-create|pr-status|pr-merge
agent-tui plan-submit|plan-dry-run|review|evolve|reset-batch|clean-verify
agent-tui tasks|events|memory-search|memory-reindex
agent-tui verify-loop|verify-live|serve
```

留言板 `!` 命令与上表对应（`!merge-all` 非交互；`!merge` 在 TUI 内已禁用）。

详见 [docs/WORKFLOW.md §7–§8](docs/WORKFLOW.md#7-cli-命令一览)。

## 诊断

```powershell
agent-tui --check --project-dir D:\your-project
cargo run --release --example pty_probe
```

## License

MIT
