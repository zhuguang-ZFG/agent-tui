//! Project-level expert agents (`.agents/specialists/*.md`) — isolated briefings on delegate.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Specialist {
    pub id: String,
    pub worker: String,
    pub triggers: Vec<String>,
    pub body: String,
}

fn specialists_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/specialists")
}

fn parse_frontmatter(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = trimmed.strip_prefix("---")?.trim_start();
    let end = rest.find("\n---")?;
    let fm = &rest[..end];
    let body = rest[end + 4..].trim();
    Some((fm.to_string(), body.to_string()))
}

fn parse_kv_line(line: &str) -> Option<(String, String)> {
    let (k, v) = line.split_once(':')?;
    Some((k.trim().to_lowercase(), v.trim().to_string()))
}

fn parse_specialist_file(path: &Path) -> Option<Specialist> {
    let raw = fs::read_to_string(path).ok()?;
    let (fm, body) = parse_frontmatter(&raw)?;
    let mut id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("specialist")
        .to_string();
    let mut worker = String::new();
    let mut triggers = Vec::new();
    for line in fm.lines() {
        let Some((k, v)) = parse_kv_line(line) else {
            continue;
        };
        match k.as_str() {
            "id" => id = v,
            "worker" => worker = v,
            "triggers" | "when" | "match" => {
                triggers = v
                    .split([',', '，', ';', ' '])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    if worker.is_empty() || triggers.is_empty() {
        return None;
    }
    Some(Specialist {
        id,
        worker,
        triggers,
        body,
    })
}

pub fn load_all(project_dir: &Path) -> Vec<Specialist> {
    let dir = specialists_dir(project_dir);
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Some(spec) = parse_specialist_file(&path) {
            out.push(spec);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn match_specialist<'a>(
    specs: &'a [Specialist],
    task: &str,
    description: &str,
) -> Option<&'a Specialist> {
    let text = format!("{task} {description}").to_lowercase();
    let mut best: Option<(&Specialist, i32)> = None;
    for spec in specs {
        let mut hits = 0i32;
        for trig in &spec.triggers {
            let t = trig.to_lowercase();
            if !t.is_empty() && text.contains(&t) {
                hits += 1;
            }
        }
        if hits == 0 {
            continue;
        }
        match best {
            None => best = Some((spec, hits)),
            Some((_, prev)) if hits > prev => best = Some((spec, hits)),
            _ => {}
        }
    }
    best.map(|(s, _)| s)
}

pub fn delegate_briefing(project_dir: &Path, task: &str, description: &str) -> Option<String> {
    let specs = load_all(project_dir);
    let spec = match_specialist(&specs, task, description)?;
    Some(format!(
        "【专家·{}】{}\n\n（专家 worker 建议 @{worker}）",
        spec.id,
        spec.body.trim(),
        worker = spec.worker
    ))
}

pub fn specialist_worker_boost(project_dir: &Path, worker: &str, task: &str, description: &str) -> i32 {
    let specs = load_all(project_dir);
    let Some(spec) = match_specialist(&specs, task, description) else {
        return 0;
    };
    if worker.eq_ignore_ascii_case(&spec.worker) {
        40
    } else {
        0
    }
}

pub fn format_roster(project_dir: &Path) -> String {
    let specs = load_all(project_dir);
    if specs.is_empty() {
        return "（无专家配置，见 .agents/specialists/）".into();
    }
    specs
        .iter()
        .map(|s| {
            format!(
                "  {} → @{}  [{}]",
                s.id,
                s.worker,
                s.triggers.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn default_bug_analyzer() -> &'static str {
    r#"---
id: bug-analyzer
worker: mimo
triggers: bug,panic,崩溃,stack trace,guru,failed test,回归,blocked
---

你是缺陷分析专家。只输出：复现路径、根因假设、建议修复 task 列表。
不要直接改业务代码；重大问题用 agent-report failed 并列出阻塞项。
"#
}

pub fn default_ui_polish() -> &'static str {
    r#"---
id: ui-polish
worker: kimi
triggers: ui,前端,组件,css,tailwind,布局,响应式,动效,页面
---

你是 UI 实现专家。优先组件化、可访问性、与现有设计系统一致。
交付可运行页面 + 简短自测步骤；避免改 unrelated 后端。
"#
}

pub fn default_arch_spike() -> &'static str {
    r#"---
id: arch-spike
worker: claude
triggers: 架构,adr,trade-off,方案,spike,迁移,重构策略,blocked
---

你是架构顾问。输出对比方案、风险、推荐路径与分阶段落地建议。
不写大段实现代码；结论需可被 Lead 转成 agent-plan。
"#
}

pub fn ensure_templates(project_dir: &Path) -> Result<usize> {
    let dir = specialists_dir(project_dir);
    fs::create_dir_all(&dir)?;
    let seeds = [
        ("bug-analyzer.md", default_bug_analyzer()),
        ("ui-polish.md", default_ui_polish()),
        ("arch-spike.md", default_arch_spike()),
    ];
    let mut created = 0usize;
    for (name, body) in seeds {
        let path = dir.join(name);
        if !path.is_file() {
            fs::write(path, body)?;
            created += 1;
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_bug_specialist() {
        let spec = Specialist {
            id: "bug-analyzer".into(),
            worker: "mimo".into(),
            triggers: vec!["bug".into(), "panic".into()],
            body: "analyze".into(),
        };
        let binding = [spec];
        let m = match_specialist(&binding, "auth-bug", "fix login panic stack trace");
        assert!(m.is_some());
        assert_eq!(m.unwrap().id, "bug-analyzer");
    }
}
