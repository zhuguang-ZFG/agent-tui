//! Per-agent strength profiles — Lead delegates by capability, not round-robin.

use std::fmt::Write as _;
use std::path::Path;

use crate::config::AgentSpec;
use crate::delegation_stats;
use crate::review_gate::is_review_task;

#[derive(Debug, Clone)]
pub struct AgentProfile {
    pub name: String,
    pub role: String,
    pub engine: &'static str,
    pub strengths: Vec<&'static str>,
    pub best_for: Vec<&'static str>,
    pub avoid: Vec<&'static str>,
}

fn builtin_profile(name: &str, role: &str) -> AgentProfile {
    let lower = name.to_lowercase();
    match lower.as_str() {
        "cursor" => AgentProfile {
            name: name.into(),
            role: role.into(),
            engine: "Cursor Agent",
            strengths: vec![
                "统筹拆解",
                "架构决策",
                "跨任务编排",
                "验收续派",
                "IDE 上下文",
            ],
            best_for: vec![
                "拆任务与 agent-plan",
                "依赖排序与并行策略",
                "回执后续派/review 编排",
                "merge-ready 后合并决策",
            ],
            avoid: vec!["大段实现代码", "代替工人写功能", "自己做 review"],
        },
        "claude" => AgentProfile {
            name: name.into(),
            role: role.into(),
            engine: "Claude Code",
            strengths: vec![
                "深度推理",
                "架构权衡",
                "方案对比",
                "ADR/设计文档",
                "复杂问题二意见",
            ],
            best_for: vec![
                "技术选型与 trade-off",
                "blocked 根因分析",
                "跨模块架构评审（非代码 diff）",
                "Spike / POC 方案",
            ],
            avoid: vec!["阻塞主路径的实现", "替代 codex/kimi 赶工", "重复 mimo 的逐行 review"],
        },
        "codex" => AgentProfile {
            name: name.into(),
            role: role.into(),
            engine: "OpenAI Codex",
            strengths: vec![
                "后端/API",
                "Rust/Python/脚本",
                "重构与算法",
                "数据库与集成",
                "单元测试与修复",
            ],
            best_for: vec![
                "服务端逻辑与 CLI",
                "类型/编译错误修复",
                "性能与数据结构",
                "非 UI 的工程质量",
            ],
            avoid: vec!["精细 UI 像素级还原", "纯 CSS 动效", "代替 Lead 拆任务"],
        },
        "mimo" => AgentProfile {
            name: name.into(),
            role: role.into(),
            engine: "Mimo",
            strengths: vec![
                "代码审查",
                "测试缺口",
                "安全与边界",
                "质量门禁",
                "回归风险",
            ],
            best_for: vec![
                "{task}-review 硬门禁",
                "合并前 smoke / 审查清单",
                "failed 后的质量归因",
            ],
            avoid: vec!["新功能从零实现", "大重构主开发", "代替 Lead 续派"],
        },
        "kimi" => AgentProfile {
            name: name.into(),
            role: role.into(),
            engine: "Kimi",
            strengths: vec![
                "React/Next 前端",
                "组件与页面",
                "Tailwind/CSS",
                "交互与 UX",
                "中文文案与 i18n",
            ],
            best_for: vec![
                "UI 组件与路由",
                "样式与响应式",
                "前端状态与表单",
                "设计稿还原（组件层）",
            ],
            avoid: vec!["核心后端 API", "数据库迁移", "Rust 系统层"],
        },
        _ => match role {
            "executor" => AgentProfile {
                name: name.into(),
                role: role.into(),
                engine: "worker",
                strengths: vec!["通用实现", "脚本", "修复"],
                best_for: vec!["后端与自动化任务"],
                avoid: vec!["Lead 编排", "专职 review"],
            },
            "frontend" => AgentProfile {
                name: name.into(),
                role: role.into(),
                engine: "worker",
                strengths: vec!["UI", "组件", "样式"],
                best_for: vec!["前端任务"],
                avoid: vec!["后端核心逻辑"],
            },
            "reviewer" => AgentProfile {
                name: name.into(),
                role: role.into(),
                engine: "reviewer",
                strengths: vec!["审查", "测试", "质量"],
                best_for: vec!["{task}-review"],
                avoid: vec!["主路径实现"],
            },
            "advisor" => AgentProfile {
                name: name.into(),
                role: role.into(),
                engine: "advisor",
                strengths: vec!["咨询", "架构", "方案"],
                best_for: vec!["非阻塞咨询"],
                avoid: vec!["赶工实现"],
            },
            _ => AgentProfile {
                name: name.into(),
                role: role.into(),
                engine: "worker",
                strengths: vec!["按任务执行"],
                best_for: vec!["Lead 指定的专项"],
                avoid: vec!["越权编排"],
            },
        },
    }
}

pub fn profile_for(spec: &AgentSpec) -> AgentProfile {
    builtin_profile(&spec.name, &spec.role)
}

/// Keyword buckets for scoring task text.
fn score_text(text: &str) -> [i32; 5] {
    let t = text.to_lowercase();
    let mut s = [0i32; 5]; // 0=frontend 1=backend 2=review 3=advisor 4=docs
    let bump = |s: &mut [i32; 5], idx: usize, n: i32| s[idx] += n;

    for kw in [
        "ui", "前端", "component", "react", "next", "vue", "css", "tailwind", "样式", "页面",
        "layout", "button", "modal", "ux", "组件", "landing", "dashboard", "figma",
    ] {
        if t.contains(kw) {
            bump(&mut s, 0, 3);
        }
    }
    for kw in [
        "api", "backend", "后端", "rust", "python", "sql", "database", "server", "endpoint",
        "grpc", "接口", "重构", "migrate", "cli", "cargo", "算法", "infra", "脚本",
    ] {
        if t.contains(kw) {
            bump(&mut s, 1, 3);
        }
    }
    for kw in [
        "review", "审查", "coverage", "测试覆盖", "security", "lint", "quality", "门禁",
        "regression", "smoke", "audit",
    ] {
        if t.contains(kw) {
            bump(&mut s, 2, 4);
        }
    }
    for kw in [
        "架构", "architecture", "design", "trade-off", "方案", "adr", "咨询", "spike", "权衡",
        "blocked", "根因",
    ] {
        if t.contains(kw) {
            bump(&mut s, 3, 3);
        }
    }
    for kw in ["doc", "readme", "文档", "changelog", "注释"] {
        if t.contains(kw) {
            bump(&mut s, 4, 2);
        }
    }
    s
}

fn agent_bucket_scores(name: &str, role: &str) -> [i32; 5] {
    let lower = name.to_lowercase();
    match lower.as_str() {
        "kimi" => [12, 2, 1, 0, 3],
        "codex" => [1, 12, 3, 1, 2],
        "mimo" => [0, 1, 15, 1, 1],
        "claude" => [0, 2, 2, 12, 4],
        "cursor" => [0, 0, 0, 8, 2],
        _ => match role {
            "frontend" => [10, 1, 0, 0, 2],
            "executor" => [1, 10, 2, 0, 1],
            "reviewer" => [0, 0, 12, 0, 0],
            "advisor" => [0, 1, 1, 10, 2],
            "architect" => [0, 0, 0, 8, 1],
            _ => [3, 3, 2, 2, 1],
        },
    }
}

/// Fit score for assigning `worker` to task+description (higher = better match).
pub fn worker_fit_score(
    worker: &str,
    role: &str,
    task: &str,
    description: &str,
    project_dir: Option<&Path>,
) -> i32 {
    if is_review_task(task) {
        return if worker.eq_ignore_ascii_case("mimo") || role == "reviewer" {
            100
        } else {
            -50
        };
    }
    if role == "architect" || worker.eq_ignore_ascii_case("cursor") {
        return -20;
    }
    let text = format!("{task} {description}");
    let buckets = score_text(&text);
    let agent = agent_bucket_scores(worker, role);
    let mut total = 0i32;
    for i in 0..5 {
        total += buckets[i] * agent[i];
    }
    if let Some(dir) = project_dir {
        total += delegation_stats::history_bias(dir, worker, task, description);
    }
    total
}

pub fn suggest_worker<'a>(
    agents: &'a [AgentSpec],
    lead: &str,
    task: &str,
    description: &str,
    project_dir: Option<&Path>,
) -> Option<&'a AgentSpec> {
    let mut best: Option<(&AgentSpec, i32)> = None;
    for a in agents {
        if a.name.eq_ignore_ascii_case(lead) || a.role == "architect" {
            continue;
        }
        if is_review_task(task) {
            if a.role == "reviewer" || a.name.eq_ignore_ascii_case("mimo") {
                return Some(a);
            }
            continue;
        }
        let score = worker_fit_score(&a.name, &a.role, task, description, project_dir);
        if score < 0 {
            continue;
        }
        match best {
            None => best = Some((a, score)),
            Some((_, prev)) if score > prev => best = Some((a, score)),
            _ => {}
        }
    }
    best.map(|(a, _)| a)
}

pub fn delegation_mismatch(
    agents: &[AgentSpec],
    lead: &str,
    worker: &str,
    task: &str,
    description: &str,
    project_dir: Option<&Path>,
) -> Option<String> {
    if worker.eq_ignore_ascii_case(lead) {
        return Some("不能委派给 Lead 自己".into());
    }
    let assigned = agents.iter().find(|a| a.name.eq_ignore_ascii_case(worker))?;
    let assigned_score =
        worker_fit_score(&assigned.name, &assigned.role, task, description, project_dir);
    let suggested = suggest_worker(agents, lead, task, description, project_dir)?;
    if suggested.name.eq_ignore_ascii_case(worker) {
        return None;
    }
    let suggested_score =
        worker_fit_score(&suggested.name, &suggested.role, task, description, project_dir);
    if suggested_score.saturating_sub(assigned_score) < 12 {
        return None;
    }
    Some(format!(
        "【委派建议】「{task}」当前 @{worker}（匹配分 {assigned_score}），更宜 @{suggested}（{suggested_score}）。按 LEAD.md 能力表发挥各 Agent 优势。",
        suggested = suggested.name,
    ))
}

pub fn format_roster_table(agents: &[AgentSpec], lead: &str) -> String {
    let mut out = String::from(
        "| Agent | 引擎 | 角色 | 最强项 | 委派时机 | 避免 |\n|-------|------|------|--------|----------|------|\n",
    );
    for a in agents {
        if a.name.eq_ignore_ascii_case(lead) {
            continue;
        }
        let p = profile_for(a);
        let strengths: String = p.strengths.join("、");
        let best: String = p.best_for.join("；");
        let avoid: String = p.avoid.join("；");
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            p.name, p.engine, p.role, strengths, best, avoid
        );
    }
    out
}

pub fn format_delegation_guide(agents: &[AgentSpec], lead: &str) -> String {
    let reviewer = agents
        .iter()
        .find(|a| a.role == "reviewer")
        .map(|a| a.name.as_str())
        .unwrap_or("mimo");
    let executor = agents
        .iter()
        .find(|a| a.role == "executor")
        .map(|a| a.name.as_str())
        .unwrap_or("codex");
    let frontend = agents
        .iter()
        .find(|a| a.role == "frontend")
        .map(|a| a.name.as_str())
        .unwrap_or("kimi");
    let advisor = agents
        .iter()
        .find(|a| a.role == "advisor")
        .map(|a| a.name.as_str())
        .unwrap_or("claude");

    format!(
        r#"## 优势委派（Lead 必守）

**原则：你只做编排，把活派给最擅长的工人 — 并行时按专长拆，不要五个 Agent 干同一件事。**

| 任务信号 | 首选 worker | 说明 |
|----------|-------------|------|
| UI/组件/CSS/React/页面 | **{frontend}** | 前端专长；后端接口让 {executor} 先做或并行 |
| API/后端/Rust/脚本/重构/DB | **{executor}** | 工程质量与逻辑实现 |
| 实现 task 的 `done` 之后 | **{reviewer}** | TUI 硬门禁自动派 `{{task}}-review`；勿跳过 |
| 架构选型 / blocked 根因 / ADR | **{advisor}** | 咨询不阻塞主路径；主实现仍归 {executor}/{frontend} |
| 拆计划、续派、merge-ready | **{lead}（你）** | 勿把实现类 task 派给自己 |

### agent-plan 示例（按优势并行）

```agent-plan
[
  {{"worker":"{executor}","task":"auth-api","description":"登录 API + 单元测试","depends_on":[]}},
  {{"worker":"{frontend}","task":"login-ui","description":"登录页组件与表单校验","depends_on":[]}},
  {{"worker":"{reviewer}","task":"auth-api-review","description":"审查 auth-api","depends_on":["auth-api"]}}
]
```

TUI 会在委派明显错配时向 Lead 提示更优 worker（见日志与 inbox）。
"#
    )
}

pub fn format_briefing_strengths(agents: &[AgentSpec], lead: &str) -> String {
    let executor = agents
        .iter()
        .find(|a| a.role == "executor")
        .map(|a| a.name.as_str())
        .unwrap_or("codex");
    let frontend = agents
        .iter()
        .find(|a| a.role == "frontend")
        .map(|a| a.name.as_str())
        .unwrap_or("kimi");
    let reviewer = agents
        .iter()
        .find(|a| a.role == "reviewer")
        .map(|a| a.name.as_str())
        .unwrap_or("mimo");
    let advisor = agents
        .iter()
        .find(|a| a.role == "advisor")
        .map(|a| a.name.as_str())
        .unwrap_or("claude");
    format!(
        "优势委派：{executor}=后端实现 {frontend}=前端 UI {reviewer}=审查门禁 {advisor}=架构咨询；你({lead})只编排不码大段。"
    )
}

pub fn strengths_doc_body(agents: &[AgentSpec], lead: &str, project_dir: Option<&Path>) -> String {
    let history = project_dir
        .map(delegation_stats::format_history_section)
        .unwrap_or_else(|| "## 历史表现（自动更新）\n\n> 工人回执后自动积累。\n".into());
    format!(
        "# Agent 能力表（agent-tui 自动维护）\n\n\
         > Lead 委派时按 **最强项** + **历史成功率** 匹配任务。\n\n\
         {}\n\
         {}\n\n\
         {}\n",
        format_roster_table(agents, lead),
        format_delegation_guide(agents, lead),
        history
    )
}

/// Strength-aware retry: prefer next-best worker when task text has clear signals.
pub fn pick_strength_retry_worker(
    agents: &[AgentSpec],
    lead: &str,
    failed_worker: &str,
    attempt: u32,
    description: &str,
    task: &str,
    project_dir: Option<&Path>,
) -> String {
    let pool: Vec<&AgentSpec> = agents
        .iter()
        .filter(|a| !a.name.eq_ignore_ascii_case(lead) && !a.name.eq_ignore_ascii_case(failed_worker))
        .collect();
    if pool.is_empty() {
        return failed_worker.to_string();
    }
    let mut ranked: Vec<(&AgentSpec, i32)> = pool
        .iter()
        .map(|a| {
            (
                *a,
                worker_fit_score(&a.name, &a.role, task, description, project_dir),
            )
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));
    let best_score = ranked.first().map(|(_, s)| *s).unwrap_or(0);
    // Weak signal → caller should fall back to round-robin rotation.
    if best_score < 24 {
        return failed_worker.to_string();
    }
    let offset = (attempt.saturating_sub(1) as usize) % ranked.len();
    ranked[offset].0.name.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn specs() -> Vec<AgentSpec> {
        vec![
            AgentSpec {
                name: "cursor".into(),
                command: "agent".into(),
                role: "architect".into(),
                worktree: PathBuf::from("."),
            },
            AgentSpec {
                name: "codex".into(),
                command: "codex".into(),
                role: "executor".into(),
                worktree: PathBuf::from("."),
            },
            AgentSpec {
                name: "kimi".into(),
                command: "kimi".into(),
                role: "frontend".into(),
                worktree: PathBuf::from("."),
            },
            AgentSpec {
                name: "mimo".into(),
                command: "mimo".into(),
                role: "reviewer".into(),
                worktree: PathBuf::from("."),
            },
        ]
    }

    #[test]
    fn suggest_frontend_for_ui() {
        let agents = specs();
        let s = suggest_worker(&agents, "cursor", "login-ui", "React 登录页组件与 Tailwind 样式", None)
            .unwrap();
        assert_eq!(s.name, "kimi");
    }

    #[test]
    fn suggest_codex_for_api() {
        let agents = specs();
        let s = suggest_worker(&agents, "cursor", "auth-api", "Rust 后端登录 API 与 SQL", None)
            .unwrap();
        assert_eq!(s.name, "codex");
    }

    #[test]
    fn review_task_prefers_mimo() {
        let agents = specs();
        let s = suggest_worker(&agents, "cursor", "auth-api-review", "审查 API", None).unwrap();
        assert_eq!(s.name, "mimo");
    }

    #[test]
    fn mismatch_detects_ui_to_codex() {
        let agents = specs();
        let msg = delegation_mismatch(
            &agents,
            "cursor",
            "codex",
            "dashboard-ui",
            "React dashboard 组件与 CSS 动效",
            None,
        );
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("kimi"));
    }
}
