//! Delegation outcome logging and historical bias for worker scoring (runtime evolution).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::meta::inbox_timestamp_iso;
use crate::review_gate::is_review_task;

const OUTCOMES: &str = "shared/delegation_outcomes.jsonl";
const HISTORY_BEGIN: &str = "<!-- agent-tui:history -->";
const HISTORY_END: &str = "<!-- /agent-tui:history -->";

pub fn evolution_enabled() -> bool {
    std::env::var("AGENT_TUI_EVOLUTION")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn outcomes_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents").join(OUTCOMES)
}

fn strengths_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/STRENGTHS.md")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRecord {
    pub time: String,
    pub worker: String,
    pub task: String,
    pub status: String,
    pub category: String,
    #[serde(default)]
    pub lead: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CategoryStats {
    pub done: u32,
    pub failed: u32,
    pub blocked: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Default)]
pub struct WorkerStats {
    pub worker: String,
    pub overall: CategoryStats,
    pub by_category: HashMap<String, CategoryStats>,
}

/// Infer task category from task name + description (same buckets as agent_strengths).
pub fn infer_category(task: &str, description: &str) -> String {
    if is_review_task(task) {
        return "review".into();
    }
    let text = format!("{task} {description}").to_lowercase();
    let mut scores: [(&str, i32); 5] = [
        ("frontend", 0),
        ("backend", 0),
        ("review", 0),
        ("advisor", 0),
        ("docs", 0),
    ];

    for kw in ["ui", "前端", "react", "css", "tailwind", "组件", "页面", "dashboard"] {
        if text.contains(kw) {
            scores[0].1 += 3;
        }
    }
    for kw in ["api", "后端", "rust", "sql", "server", "接口", "cargo", "脚本"] {
        if text.contains(kw) {
            scores[1].1 += 3;
        }
    }
    for kw in ["review", "审查", "smoke", "audit"] {
        if text.contains(kw) {
            scores[2].1 += 3;
        }
    }
    for kw in ["架构", "adr", "方案", "咨询", "spike"] {
        if text.contains(kw) {
            scores[3].1 += 3;
        }
    }
    for kw in ["doc", "readme", "文档"] {
        if text.contains(kw) {
            scores[4].1 += 2;
        }
    }

    scores
        .iter()
        .max_by_key(|(_, s)| *s)
        .filter(|(_, s)| *s > 0)
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| "general".into())
}

pub fn load_outcomes(project_dir: &Path, limit: usize) -> Vec<OutcomeRecord> {
    let path = outcomes_path(project_dir);
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn bump(stats: &mut CategoryStats, status: &str) {
    stats.total += 1;
    match status {
        "done" => stats.done += 1,
        "failed" | "review_failed" => stats.failed += 1,
        "blocked" => stats.blocked += 1,
        _ => {}
    }
}

pub fn aggregate(project_dir: &Path) -> HashMap<String, WorkerStats> {
    let mut map: HashMap<String, WorkerStats> = HashMap::new();
    for rec in load_outcomes(project_dir, 10_000) {
        if !matches!(
            rec.status.as_str(),
            "done" | "failed" | "blocked" | "review_failed"
        ) {
            continue;
        }
        let entry = map
            .entry(rec.worker.clone())
            .or_insert_with(|| WorkerStats {
                worker: rec.worker.clone(),
                ..Default::default()
            });
        bump(&mut entry.overall, &rec.status);
        bump(
            entry
                .by_category
                .entry(rec.category.clone())
                .or_default(),
            &rec.status,
        );
    }
    map
}

pub fn record_outcome(
    project_dir: &Path,
    worker: &str,
    task: &str,
    status: &str,
    description: &str,
    lead: &str,
    summary: &str,
) -> Result<()> {
    if !evolution_enabled() {
        return Ok(());
    }
    if !matches!(status, "done" | "failed" | "blocked" | "review_failed") {
        return Ok(());
    }
    let path = outcomes_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let rec = OutcomeRecord {
        time: inbox_timestamp_iso(),
        worker: worker.to_string(),
        task: task.to_string(),
        status: status.to_string(),
        category: infer_category(task, description),
        lead: Some(lead.to_string()),
        summary: Some(summary.chars().take(200).collect()),
    };
    let line = serde_json::to_string(&rec)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

fn success_rate(stats: &CategoryStats) -> Option<f32> {
    let attempts = stats.done + stats.failed;
    if attempts < 3 {
        return None;
    }
    Some(stats.done as f32 / attempts as f32)
}

/// Historical bias for worker_fit_score: roughly -30..+30 from past success in category.
pub fn history_bias(
    project_dir: &Path,
    worker: &str,
    task: &str,
    description: &str,
) -> i32 {
    if !evolution_enabled() {
        return 0;
    }
    let cat = infer_category(task, description);
    let agg = aggregate(project_dir);
    let Some(ws) = agg.get(worker) else {
        return 0;
    };
    let stats = ws
        .by_category
        .get(&cat)
        .or(Some(&ws.overall))
        .unwrap();
    let Some(rate) = success_rate(stats) else {
        return 0;
    };
    ((rate - 0.5) * 60.0).round() as i32
}

pub fn format_history_section(project_dir: &Path) -> String {
    let agg = aggregate(project_dir);
    if agg.is_empty() {
        return "## 历史表现（自动更新）\n\n> 尚无委派结果记录。工人回执 `done/failed` 后会自动积累。\n"
            .into();
    }

    let mut workers: Vec<_> = agg.values().collect();
    workers.sort_by(|a, b| a.worker.cmp(&b.worker));

    let mut out = String::from(
        "## 历史表现（自动更新）\n\n\
         > 由 `delegation_outcomes.jsonl` 汇总；`suggest_worker` 与重试会参考成功率（≥3 次样本）。\n\n\
         | Worker | 完成 | 失败 | 受阻 | 综合成功率 |\n|--------|------|------|------|------------|\n",
    );
    for ws in &workers {
        let rate = success_rate(&ws.overall)
            .map(|r| format!("{:.0}%", r * 100.0))
            .unwrap_or_else(|| "样本不足".into());
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            ws.worker, ws.overall.done, ws.overall.failed, ws.overall.blocked, rate
        );
    }

    out.push_str("\n### 分领域（样本≥3）\n\n");
    for ws in workers {
        let mut cats: Vec<_> = ws.by_category.iter().collect();
        cats.sort_by(|a, b| a.0.cmp(b.0));
        let mut lines = Vec::new();
        for (cat, st) in cats {
            if let Some(r) = success_rate(st) {
                lines.push(format!(
                    "{} {:.0}%（{}/{}）",
                    cat,
                    r * 100.0,
                    st.done,
                    st.done + st.failed
                ));
            }
        }
        if !lines.is_empty() {
            let _ = writeln!(out, "- **{}**：{}", ws.worker, lines.join("；"));
        }
    }
    out
}

pub fn patch_strengths_history(project_dir: &Path) -> Result<bool> {
    let path = strengths_path(project_dir);
    let history = format!(
        "{HISTORY_BEGIN}\n{}\n{HISTORY_END}\n",
        format_history_section(project_dir).trim_end()
    );
    let body = if path.is_file() {
        let existing = fs::read_to_string(&path)?;
        if existing.contains(HISTORY_BEGIN) && existing.contains(HISTORY_END) {
            let before = existing
                .split(HISTORY_BEGIN)
                .next()
                .unwrap_or("")
                .trim_end();
            let after = existing
                .split(HISTORY_END)
                .nth(1)
                .unwrap_or("")
                .trim_start();
            format!("{before}\n\n{history}\n{after}")
        } else {
            format!("{existing}\n\n{history}")
        }
    } else {
        history
    };
    fs::write(&path, body.trim_end().to_string() + "\n")?;
    Ok(true)
}

/// Recompute stats + patch STRENGTHS.md; returns outcome count.
pub fn evolve_project(project_dir: &Path) -> Result<usize> {
    let n = load_outcomes(project_dir, 10_000).len();
    patch_strengths_history(project_dir)?;
    Ok(n)
}

pub fn maybe_auto_evolve(project_dir: &Path) -> Result<()> {
    if !evolution_enabled() {
        return Ok(());
    }
    let n = load_outcomes(project_dir, 10_000).len();
    if n > 0 && n % 10 == 0 {
        let _ = patch_strengths_history(project_dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_categories() {
        assert_eq!(infer_category("login-ui", "React 页面"), "frontend");
        assert_eq!(infer_category("auth-api", "Rust 后端 API"), "backend");
        assert_eq!(infer_category("x-review", "审查"), "review");
    }
}
