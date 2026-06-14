//! `agent-tui plan <task>` — auto-decompose a large task into sub-tasks
//! with code-ownership-aware worker assignment.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::agent_strengths;
use crate::config::{load_agents, AgentSpec};
use crate::lead_watch::PlanItem;
use crate::task_dag;

/// Ownership map: directory pattern → (role, agent_name)
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OwnerEntry {
    pub dir_pattern: String,
    pub role: String,
    pub keywords: Vec<String>,
}

/// Build a code ownership map from project structure.
#[allow(dead_code)]
pub fn build_ownership_map(project_dir: &Path) -> Result<Vec<OwnerEntry>> {
    let scan = crate::auto_route::scan_and_generate(project_dir)?;
    let agents = load_agents(project_dir)?;
    let mut entries = Vec::new();

    for (role, dirs) in &scan.role_dirs {
        // Find the agent with this role
        let agent = agents.iter().find(|a| a.role.eq_ignore_ascii_case(role));
        if let Some(_agent) = agent {
            for dir in dirs {
                entries.push(OwnerEntry {
                    dir_pattern: dir.clone(),
                    role: role.clone(),
                    keywords: Vec::new(),
                });
            }
        }
    }
    Ok(entries)
}

/// Decomposition hint: task type → typical sub-task template
const TASK_TEMPLATES: &[(&[&str], &str, &str, &[&str])] = &[
    // Feature implementation
    (&["feature", "功能", "实现", "implement", "add", "new"],
     "implement",
     "实现 {name}",
     &["api", "ui", "test", "doc"]),
    // Bug fix
    (&["fix", "修复", "bug", "error", "crash", "issue"],
     "fix",
     "修复 {name}",
     &["diagnose", "fix", "test", "verify"]),
    // Refactor
    (&["refactor", "重构", "clean", "cleanup", "restructure"],
     "refactor",
     "重构 {name}",
     &["analyze", "refactor", "test", "review"]),
    // Review/audit
    (&["review", "审查", "audit", "check", "quality"],
     "review",
     "审查 {name}",
     &["analyze", "review"]),
    // CI/DevOps
    (&["ci", "cd", "deploy", "docker", "pipeline", "部署"],
     "devops",
     "配置 {name}",
     &["config", "test", "verify"]),
    // Documentation
    (&["doc", "文档", "readme", "guide", "adr"],
     "docs",
     "编写 {name}",
     &["outline", "write", "review"]),
];

/// Phase templates for sub-task generation
#[derive(Debug, Clone)]
struct PhaseTemplate {
    id: &'static str,
    name_template: &'static str,
    role_hint: &'static str,
    depends_on: Vec<&'static str>,
}

fn get_phases(task_type: &str) -> Vec<PhaseTemplate> {
    match task_type {
        "implement" => vec![
            PhaseTemplate {
                id: "api",
                name_template: "{name}-api",
                role_hint: "executor",
                depends_on: vec![],
            },
            PhaseTemplate {
                id: "ui",
                name_template: "{name}-ui",
                role_hint: "frontend",
                depends_on: vec!["api"],
            },
            PhaseTemplate {
                id: "test",
                name_template: "{name}-test",
                role_hint: "tester",
                depends_on: vec!["api", "ui"],
            },
            PhaseTemplate {
                id: "doc",
                name_template: "{name}-doc",
                role_hint: "analyst",
                depends_on: vec!["api"],
            },
        ],
        "fix" => vec![
            PhaseTemplate {
                id: "diagnose",
                name_template: "{name}-diagnose",
                role_hint: "advisor",
                depends_on: vec![],
            },
            PhaseTemplate {
                id: "fix",
                name_template: "{name}-fix",
                role_hint: "executor",
                depends_on: vec!["diagnose"],
            },
            PhaseTemplate {
                id: "test",
                name_template: "{name}-test",
                role_hint: "tester",
                depends_on: vec!["fix"],
            },
            PhaseTemplate {
                id: "verify",
                name_template: "{name}-verify",
                role_hint: "reviewer",
                depends_on: vec!["fix"],
            },
        ],
        "refactor" => vec![
            PhaseTemplate {
                id: "analyze",
                name_template: "{name}-analyze",
                role_hint: "analyst",
                depends_on: vec![],
            },
            PhaseTemplate {
                id: "refactor",
                name_template: "{name}-refactor",
                role_hint: "executor",
                depends_on: vec!["analyze"],
            },
            PhaseTemplate {
                id: "test",
                name_template: "{name}-test",
                role_hint: "tester",
                depends_on: vec!["refactor"],
            },
            PhaseTemplate {
                id: "review",
                name_template: "{name}-review",
                role_hint: "reviewer",
                depends_on: vec!["refactor"],
            },
        ],
        "review" => vec![
            PhaseTemplate {
                id: "analyze",
                name_template: "{name}-analyze",
                role_hint: "analyst",
                depends_on: vec![],
            },
            PhaseTemplate {
                id: "review",
                name_template: "{name}-review",
                role_hint: "reviewer",
                depends_on: vec!["analyze"],
            },
        ],
        "devops" => vec![
            PhaseTemplate {
                id: "config",
                name_template: "{name}-config",
                role_hint: "integrator",
                depends_on: vec![],
            },
            PhaseTemplate {
                id: "test",
                name_template: "{name}-test",
                role_hint: "tester",
                depends_on: vec!["config"],
            },
            PhaseTemplate {
                id: "verify",
                name_template: "{name}-verify",
                role_hint: "reviewer",
                depends_on: vec!["config"],
            },
        ],
        "docs" => vec![
            PhaseTemplate {
                id: "outline",
                name_template: "{name}-outline",
                role_hint: "analyst",
                depends_on: vec![],
            },
            PhaseTemplate {
                id: "write",
                name_template: "{name}-write",
                role_hint: "analyst",
                depends_on: vec!["outline"],
            },
            PhaseTemplate {
                id: "review",
                name_template: "{name}-review",
                role_hint: "reviewer",
                depends_on: vec!["write"],
            },
        ],
        _ => vec![
            PhaseTemplate {
                id: "implement",
                name_template: "{name}-impl",
                role_hint: "executor",
                depends_on: vec![],
            },
            PhaseTemplate {
                id: "review",
                name_template: "{name}-review",
                role_hint: "reviewer",
                depends_on: vec!["implement"],
            },
        ],
    }
}

/// Classify a task by keywords in its description.
fn classify_task(task: &str, description: &str) -> &'static str {
    let text = format!("{task} {description}").to_lowercase();
    for (keywords, task_type, _, _) in TASK_TEMPLATES {
        if keywords.iter().any(|kw| text.contains(kw)) {
            return task_type;
        }
    }
    "implement" // default
}

/// Extract the core name from a task string.
/// "auth-api" → "auth-api", "实现登录功能" → "login"
fn extract_name(task: &str) -> &str {
    task.trim()
}

/// Find the best worker for a role hint, considering project routing and agent strengths.
fn find_worker_for_role(
    agents: &[AgentSpec],
    lead: &str,
    role_hint: &str,
    task: &str,
    description: &str,
    project_dir: &Path,
) -> Option<String> {
    // First: try exact role match
    let role_match = agents.iter().find(|a| {
        a.role.eq_ignore_ascii_case(role_hint) && !a.name.eq_ignore_ascii_case(lead)
    });
    if let Some(a) = role_match {
        return Some(a.name.clone());
    }

    // Second: use agent strengths scoring
    let pool: Vec<&AgentSpec> = agents
        .iter()
        .filter(|a| !a.name.eq_ignore_ascii_case(lead) && a.role != "architect")
        .collect();

    let mut best: Option<(&AgentSpec, i32)> = None;
    for a in &pool {
        let score = agent_strengths::worker_fit_score(
            &a.name,
            &a.role,
            task,
            description,
            Some(project_dir),
        );
        match best {
            None => best = Some((a, score)),
            Some((_, prev)) if score > prev => best = Some((a, score)),
            _ => {}
        }
    }
    best.map(|(a, _)| a.name.clone())
}

/// Result of plan generation.
pub struct GeneratedPlan {
    pub items: Vec<PlanItem>,
    pub task_type: String,
    pub summary: String,
}

/// Generate a decomposed plan for a large task.
pub fn generate_plan(
    project_dir: &Path,
    task: &str,
    description: &str,
    lead: &str,
) -> Result<GeneratedPlan> {
    let agents = load_agents(project_dir)?;
    let task_type = classify_task(task, description).to_string();
    let name = extract_name(task);
    let phases = get_phases(&task_type);

    let mut items = Vec::new();
    let mut assigned = BTreeMap::<String, String>::new(); // id → worker

    for phase in &phases {
        let sub_task = phase.name_template.replace("{name}", name);
        let sub_desc = format!("{} 的子任务: {}", task, phase.id);

        // Find worker for this role
        let worker = find_worker_for_role(
            &agents,
            lead,
            phase.role_hint,
            &sub_task,
            &sub_desc,
            project_dir,
        )
        .unwrap_or_else(|| {
            // Fallback: pick first non-lead worker
            agents
                .iter()
                .find(|a| !a.name.eq_ignore_ascii_case(lead) && a.role != "architect")
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "claude".into())
        });

        // Resolve depends_on: map phase ids to actual task names
        let deps: Vec<String> = phase
            .depends_on
            .iter()
            .filter_map(|dep_id| {
                assigned.get(*dep_id).cloned().or_else(|| {
                    // Find the task name for this dep id
                    phases
                        .iter()
                        .find(|p| p.id == *dep_id)
                        .map(|p| p.name_template.replace("{name}", name))
                })
            })
            .collect();

        assigned.insert(phase.id.to_string(), sub_task.clone());

        items.push(PlanItem {
            worker,
            task: sub_task,
            description: sub_desc,
            depends_on: deps,
        });
    }

    let summary = format!(
        "任务类型: {task_type} | 拆解为 {} 个子任务 | 覆盖 {} 个 agent",
        items.len(),
        {
            let workers: std::collections::BTreeSet<&str> =
                items.iter().map(|i| i.worker.as_str()).collect();
            workers.len()
        }
    );

    Ok(GeneratedPlan {
        items,
        task_type,
        summary,
    })
}

/// Format a generated plan as agent-plan JSON (for Lead to output).
pub fn format_plan_json(plan: &GeneratedPlan) -> String {
    let items_json: Vec<String> = plan
        .items
        .iter()
        .map(|item| {
            let deps: Vec<String> = item
                .depends_on
                .iter()
                .map(|d| format!("\"{}\"", d))
                .collect();
            format!(
                "  {{\"worker\":\"{}\",\"task\":\"{}\",\"description\":\"{}\",\"depends_on\":[{}]}}",
                item.worker,
                item.task,
                item.description.replace('"', "\\\""),
                deps.join(",")
            )
        })
        .collect();
    format!("```agent-plan\n[\n{}\n]\n```", items_json.join(",\n"))
}

/// Format a human-readable plan summary.
pub fn format_plan_summary(plan: &GeneratedPlan) -> String {
    let mut out = String::from("\n── 智能拆解计划 ──\n");
    out.push_str(&format!("  类型: {}\n", plan.task_type));
    out.push_str(&format!("  子任务: {} 个\n\n", plan.items.len()));

    for (i, item) in plan.items.iter().enumerate() {
        let deps = if item.depends_on.is_empty() {
            "无依赖".to_string()
        } else {
            format!("等待: {}", item.depends_on.join(", "))
        };
        out.push_str(&format!(
            "  {}. [{}] {} → @{}\n     {}\n",
            i + 1,
            deps,
            item.task,
            item.worker,
            item.description
        ));
    }

    out.push_str(&format!("\n  {}\n", plan.summary));
    out
}

/// Generate plan + schedule it via the task DAG.
pub fn plan_and_dispatch(
    project_dir: &Path,
    task: &str,
    description: &str,
    lead: &str,
    dry_run: bool,
) -> Result<String> {
    let plan = generate_plan(project_dir, task, description, lead)?;
    let summary = format_plan_summary(&plan);
    let json = format_plan_json(&plan);

    let mut log = summary;

    if dry_run {
        log.push_str("\n── Dry Run: agent-plan JSON ──\n");
        log.push_str(&json);
    } else {
        // Schedule via task DAG
        let completed = task_dag::load_completed_tasks(project_dir);
        let (ready, pending) =
            task_dag::schedule_plans(project_dir, plan.items.clone(), &completed)?;

        log.push_str(&format!(
            "\n  ✓ 已调度: {} 个就绪, {} 个待依赖\n",
            ready.len(),
            pending.len()
        ));

        // Auto-delegate ready items
        for item in &ready {
            if item.worker.eq_ignore_ascii_case(lead) {
                continue;
            }
            match crate::delegation::delegate_task(
                project_dir,
                lead,
                &item.worker,
                &item.task,
                &item.description,
            ) {
                Ok(()) => {
                    log.push_str(&format!("  → 已委派 {} → @{}\n", item.task, item.worker));
                }
                Err(e) => {
                    log.push_str(&format!("  ⚠ 委派 {} 失败: {e:#}\n", item.task));
                }
            }
        }
    }

    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn setup_test_project() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plan-gen-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let agents_dir = dir.join(".agents");
        fs::create_dir_all(&agents_dir).unwrap();
        fs::write(
            agents_dir.join("agents.yaml"),
            r#"agents:
  cursor:
    command: cursor
    role: architect
  codex:
    command: codex
    role: executor
  kimi:
    command: kimi
    role: frontend
  mimo:
    command: mimo
    role: reviewer
  kilo:
    command: kilo
    role: tester
"#,
        )
        .unwrap();
        dir
    }

    #[test]
    fn classify_feature_task() {
        assert_eq!(classify_task("login-ui", "React 登录页组件"), "implement");
    }

    #[test]
    fn classify_bugfix_task() {
        assert_eq!(classify_task("fix-crash", "修复空指针 crash"), "fix");
    }

    #[test]
    fn classify_refactor_task() {
        assert_eq!(classify_task("refactor-auth", "重构认证模块"), "refactor");
    }

    #[test]
    fn generate_impl_plan() {
        let dir = setup_test_project();
        let plan = generate_plan(&dir, "auth", "实现登录注册 API 和 UI", "cursor").unwrap();
        assert!(plan.items.len() >= 2);
        // Check that at least one item is assigned to codex (executor)
        assert!(plan.items.iter().any(|i| i.worker == "codex"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_fix_plan_has_diagnose() {
        let dir = setup_test_project();
        let plan = generate_plan(&dir, "fix-crash", "修复 OOM crash", "cursor").unwrap();
        assert!(plan.task_type == "fix");
        assert!(plan.items.iter().any(|i| i.task.contains("diagnose")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_review_plan_is_short() {
        let dir = setup_test_project();
        let plan =
            generate_plan(&dir, "auth-review", "审查认证代码", "cursor").unwrap();
        assert!(plan.task_type == "review");
        assert!(plan.items.len() <= 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_json_format_valid() {
        let dir = setup_test_project();
        let plan = generate_plan(&dir, "test-task", "实现测试功能", "cursor").unwrap();
        let json = format_plan_json(&plan);
        assert!(json.contains("agent-plan"));
        assert!(json.contains("\"worker\""));
        assert!(json.contains("\"task\""));
        assert!(json.contains("\"depends_on\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deps_reference_valid_tasks() {
        let dir = setup_test_project();
        let plan = generate_plan(&dir, "auth", "实现认证", "cursor").unwrap();
        let task_names: std::collections::HashSet<&str> =
            plan.items.iter().map(|i| i.task.as_str()).collect();
        for item in &plan.items {
            for dep in &item.depends_on {
                assert!(
                    task_names.contains(dep.as_str()),
                    "dep {dep} not in tasks {:?}",
                    task_names
                );
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
