//! Optional project-level routing overrides (`.agents/routing.yaml`).

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct RoutingFile {
    #[serde(default)]
    rules: Vec<RoutingRule>,
}

#[derive(Debug, Clone, Deserialize)]
struct RoutingRule {
    #[serde(default)]
    r#match: Vec<String>,
    worker: String,
    #[serde(default)]
    bonus: i32,
}

fn routing_path(project_dir: &Path) -> std::path::PathBuf {
    project_dir.join(".agents/routing.yaml")
}

pub fn load_rules(project_dir: &Path) -> Vec<(Vec<String>, String, i32)> {
    let path = routing_path(project_dir);
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(file) = serde_yaml::from_str::<RoutingFile>(&text) else {
        return Vec::new();
    };
    file.rules
        .into_iter()
        .map(|r| {
            let bonus = if r.bonus == 0 { 24 } else { r.bonus };
            (r.r#match, r.worker, bonus)
        })
        .collect()
}

/// Bonus score when task text matches a project routing rule for this worker.
pub fn project_routing_bonus(project_dir: &Path, worker: &str, task: &str, description: &str) -> i32 {
    let text = format!("{task} {description}").to_lowercase();
    let mut total = 0i32;
    for (keywords, target, bonus) in load_rules(project_dir) {
        if !worker.eq_ignore_ascii_case(&target) {
            continue;
        }
        if keywords.iter().any(|kw| text.contains(&kw.to_lowercase())) {
            total += bonus;
        }
    }
    total
}

pub fn default_routing_yaml() -> &'static str {
    r#"# 任务类型 → Agent 路由（可选，覆盖/增强内置 STRENGTHS 评分）
# match 关键词命中且 worker 匹配时加分；Lead plan 仍可指定任意 worker。
rules:
  - match: ["ui", "react", "前端", "组件", "tailwind", "页面"]
    worker: kimi
  - match: ["api", "rust", "后端", "sql", "cli", "重构"]
    worker: codex
  - match: ["review", "审查", "smoke", "质量"]
    worker: mimo
  - match: ["架构", "adr", "blocked", "根因", "方案"]
    worker: claude
"#
}

pub fn ensure_routing_template(project_dir: &Path) -> Result<bool> {
    let path = routing_path(project_dir);
    if path.is_file() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, default_routing_yaml())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bonus_applies_for_matching_worker() {
        let dir = std::env::temp_dir().join(format!("routing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents")).unwrap();
        fs::write(
            dir.join(".agents/routing.yaml"),
            r#"rules:
  - match: ["dashboard", "ui"]
    worker: kimi
    bonus: 30
"#,
        )
        .unwrap();
        let b = project_routing_bonus(&dir, "kimi", "dash-ui", "React dashboard");
        assert!(b >= 30);
        let _ = fs::remove_dir_all(&dir);
    }
}
