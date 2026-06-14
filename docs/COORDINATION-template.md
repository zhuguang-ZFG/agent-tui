# 多 Agent 协调规则（agent-tui 模板）

> 复制到目标项目的 `.agents/COORDINATION.md`。TUI 启动时会设置 `AGENT_TUI_COORD_DOC` 指向该文件，并同步 Lead 的 IDE 规则。

## 环境变量（TUI 注入）

| 变量 | 含义 |
|------|------|
| `AGENT_TUI=1` | 在 TUI 内 |
| `AGENT_TUI_AGENT` | 你的名字 |
| `AGENT_TUI_ROLE` | architect / executor / … |
| `AGENT_TUI_LEAD` | 主 Agent 名 |
| `AGENT_TUI_PROJECT` | 项目根 |
| `AGENT_TUI_COORD_DOC` | 本文件绝对路径 |
| `AGENT_TUI_ORCHESTRATOR=1` | 仅 Lead |

## Lead 职责

1. 收到 `!任务` 或用户消息后分析需求
2. 输出 **agent-plan** → TUI 自动 delegate
3. 收到工人 **agent-report** 后 **立即续派**（review / 修复 / 下一波任务）

```agent-plan
[
  {"worker":"codex","task":"auth-api","description":"实现登录 API"},
  {"worker":"kimi","task":"login-ui","description":"登录页","depends_on":["auth-api"]}
]
```

可选 **plan_inbox** 双通道：追加 JSON 行到 `shared/plan_inbox.jsonl`。

## 工人职责

完成或受阻时输出 **agent-report**：

```agent-report
{"task":"auth-api","status":"done","summary":"已实现登录 API"}
```

`status`：`done` | `blocked` | `failed`

看到 `[协调/…]` → 读 `{agent}/memory/inbox.md` → 执行 → 回执。

## 共享路径

- `shared/inbox.md` — 留言板
- `shared/mailbox.jsonl` — 结构化信箱
- `shared/task_state.jsonl` — 任务状态
- `shared/events.jsonl` — Relay 事件
- `{agent}/memory/` — 长期记忆

## 原则

- 一个任务一个主认领人
- 工人只改自己的 worktree
- 合并前让 reviewer Agent review

完整流程见 [WORKFLOW.md](WORKFLOW.md)。
