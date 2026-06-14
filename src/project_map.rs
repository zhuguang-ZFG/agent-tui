//! Lightweight project knowledge map (Zread-inspired, filesystem scan only).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

use crate::config;

const MAP_PATH: &str = ".agents/PROJECT_MAP.md";
/// Relative path from project root (for docs / briefing).
pub const MAP_REL_PATH: &str = MAP_PATH;
const STALE_HOURS: u64 = 24;

fn map_file(project_dir: &Path) -> PathBuf {
    project_dir.join(MAP_PATH)
}

fn is_stale(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return false;
    };
    age.as_secs() > STALE_HOURS * 3600
}

fn list_dir_names(path: &Path, max: usize) -> Vec<String> {
    let Ok(rd) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.') && n != "target" && n != "node_modules")
        .collect();
    names.sort();
    names.truncate(max);
    names
}

pub fn generate(project_dir: &Path) -> Result<String> {
    let agents = config::load_agents(project_dir).unwrap_or_default();
    let lead = config::resolve_lead_agent(&agents);
    let root_entries = list_dir_names(project_dir, 24);
    let src_entries = list_dir_names(&project_dir.join("src"), 20);
    let agents_lines: Vec<String> = agents
        .iter()
        .map(|a| format!("- **{}** (`{}`) — {}", a.name, a.role, a.command))
        .collect();

    let mut stack: Vec<String> = Vec::new();
    if project_dir.join("Cargo.toml").is_file() {
        stack.push("Rust / Cargo".into());
    }
    if project_dir.join("package.json").is_file() {
        stack.push("Node.js".into());
    }
    if project_dir.join("pyproject.toml").is_file() || project_dir.join("requirements.txt").is_file() {
        stack.push("Python".into());
    }
    if stack.is_empty() {
        stack.push("（未检测到常见 manifest）".into());
    }

    let specialists = crate::specialists::format_roster(project_dir);
    let routing_hint = if project_dir.join(".agents/routing.yaml").is_file() {
        "已配置 `.agents/routing.yaml`"
    } else {
        "使用内置 STRENGTHS 路由"
    };

    let body = format!(
        "# PROJECT_MAP（agent-tui 自动生成）\n\n\
         > 供 Lead / Reviewer 快速理解仓库布局。可 `!map` 或 merge-ready 后自动刷新。\n\n\
         ## 概览\n\n\
         - 项目根: `{}`\n\
         - Lead: **{lead}**\n\
         - 技术栈: {}\n\
         - 路由: {routing_hint}\n\n\
         ## 顶层目录\n\n\
         {}\n\n\
         ## src/（若存在）\n\n\
         {}\n\n\
         ## Agent 名册\n\n\
         {}\n\n\
         ## 专家子 Agent（specialists）\n\n\
         {}\n\n\
         ## 协调入口\n\n\
         - `.agents/LEAD.md` — Lead Playbook\n\
         - `.agents/STRENGTHS.md` — 能力表 + 历史表现\n\
         - `.agents/shared/task_state.jsonl` — 任务状态机\n",
        project_dir.display(),
        stack.join(", "),
        if root_entries.is_empty() {
            "- （空）".into()
        } else {
            root_entries
                .iter()
                .map(|n| format!("- `{n}/`"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        if src_entries.is_empty() {
            "- （无 src/ 或未列出）".into()
        } else {
            src_entries
                .iter()
                .map(|n| format!("- `{n}`"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        if agents_lines.is_empty() {
            "- （无 agents.yaml）".into()
        } else {
            agents_lines.join("\n")
        },
        specialists
    );

    let path = map_file(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &body)?;
    Ok(format!(
        "已生成 {}（{} 行）",
        path.display(),
        body.lines().count()
    ))
}

/// Refresh if missing or older than 24h; returns message when regenerated.
pub fn maybe_refresh(project_dir: &Path) -> Result<Option<String>> {
    let path = map_file(project_dir);
    if path.is_file() && !is_stale(&path) {
        return Ok(None);
    }
    let msg = generate(project_dir)?;
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_map_file() {
        let dir = std::env::temp_dir().join(format!("proj-map-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname=\"demo\"\n").unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        let msg = generate(&dir).unwrap();
        assert!(msg.contains("PROJECT_MAP"));
        assert!(dir.join(".agents/PROJECT_MAP.md").is_file());
        let _ = fs::remove_dir_all(&dir);
    }
}
