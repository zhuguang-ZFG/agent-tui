//! Cross-agent memory aggregation — unified view of all agents' knowledge.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::meta::inbox_timestamp_iso;

/// A single memory entry from one agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub agent: String,
    pub role: String,
    pub source: String, // "memory" | "checkpoint" | "notes"
    pub content: String,
    pub timestamp: String,
}

/// Aggregated memory across all agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedMemorySnapshot {
    pub generated_at: String,
    pub agents: Vec<AgentMemorySummary>,
    pub entries: Vec<MemoryEntry>,
    pub stats: MemoryStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMemorySummary {
    pub name: String,
    pub role: String,
    pub memory_exists: bool,
    pub checkpoint_exists: bool,
    pub notes_exists: bool,
    pub assignment_count: usize,
    pub durable_count: usize,
    pub last_checkpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total_agents: usize,
    pub agents_with_memory: usize,
    pub total_entries: usize,
    pub total_durable: usize,
}

/// Extract durable knowledge lines from MEMORY.md content.
fn extract_durable_entries(content: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut in_durable = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "<!-- agent-tui:durable -->" {
            in_durable = true;
            continue;
        }
        if trimmed == "<!-- /agent-tui:durable -->" {
            break;
        }
        if in_durable && trimmed.starts_with("- ") {
            let text = trimmed.trim_start_matches("- ").to_string();
            // Extract timestamp if present: [2024-01-15T10:30:00] content
            let (ts, content) = if text.starts_with('[') {
                if let Some(bracket_end) = text.find(']') {
                    (
                        text[1..bracket_end].to_string(),
                        text[bracket_end + 1..].trim().to_string(),
                    )
                } else {
                    (String::new(), text)
                }
            } else {
                (String::new(), text)
            };
            entries.push((ts, content));
        }
    }
    entries
}

/// Extract assignment count from MEMORY.md content.
fn extract_assignment_count(content: &str) -> usize {
    let mut in_assignments = false;
    let mut count = 0;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "<!-- agent-tui:assignments -->" {
            in_assignments = true;
            continue;
        }
        if trimmed == "<!-- /agent-tui:assignments -->" {
            break;
        }
        if in_assignments && trimmed.starts_with('|') && !trimmed.contains("Task |") && !trimmed.contains("---") {
            count += 1;
        }
    }
    count
}

/// Extract last checkpoint update time.
fn extract_checkpoint_time(content: &str) -> Option<String> {
    for line in content.lines() {
        if line.starts_with("Updated: ") {
            let rest = line.trim_start_matches("Updated: ");
            if let Some(end) = rest.find(" |") {
                return Some(rest[..end].to_string());
            }
            return Some(rest.to_string());
        }
    }
    None
}

/// Extract recent notes entries.
fn extract_notes_entries(content: &str, max: usize) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();
    for line in content.lines() {
        if line.starts_with("## [") {
            if !current.is_empty() {
                entries.push(current.clone());
                current.clear();
            }
            current = line.to_string();
        } else if !current.is_empty() && !line.trim().is_empty() {
            current.push(' ');
            current.push_str(line.trim());
        }
    }
    if !current.is_empty() {
        entries.push(current);
    }
    entries.into_iter().rev().take(max).collect()
}

/// Build a cross-agent memory snapshot.
pub fn build_shared_memory(project_dir: &Path) -> Result<SharedMemorySnapshot> {
    let agents = config::load_agents(project_dir).unwrap_or_default();
    let mut agent_summaries = Vec::new();
    let mut all_entries = Vec::new();
    let mut agents_with_memory = 0usize;
    let mut total_durable = 0usize;

    for agent in &agents {
        let memory_path = project_dir.join(format!(".agents/{}/memory/MEMORY.md", agent.name));
        let checkpoint_path =
            project_dir.join(format!(".agents/{}/memory/checkpoint.md", agent.name));
        let notes_path = project_dir.join(format!(".agents/{}/memory/notes.md", agent.name));

        let memory_exists = memory_path.is_file();
        let checkpoint_exists = checkpoint_path.is_file();
        let notes_exists = notes_path.is_file();

        if memory_exists {
            agents_with_memory += 1;
        }

        let mut assignment_count = 0;
        let mut durable_count = 0;
        let mut last_checkpoint = None;

        // Parse MEMORY.md
        if memory_exists {
            if let Ok(content) = fs::read_to_string(&memory_path) {
                assignment_count = extract_assignment_count(&content);
                let durable = extract_durable_entries(&content);
                durable_count = durable.len();
                total_durable += durable_count;
                for (ts, text) in durable {
                    all_entries.push(MemoryEntry {
                        agent: agent.name.clone(),
                        role: agent.role.clone(),
                        source: "durable".to_string(),
                        content: text,
                        timestamp: ts,
                    });
                }
            }
        }

        // Parse checkpoint
        if checkpoint_exists {
            if let Ok(content) = fs::read_to_string(&checkpoint_path) {
                last_checkpoint = extract_checkpoint_time(&content);
                // Extract active intent as an entry
                if let Some(start) = content.find("## §1 Active intent") {
                    let rest = &content[start..];
                    let end = rest.find("## §2").unwrap_or(rest.len());
                    let intent = rest["## §1 Active intent".len()..end]
                        .trim()
                        .to_string();
                    if !intent.is_empty() && intent != "(none)" {
                        all_entries.push(MemoryEntry {
                            agent: agent.name.clone(),
                            role: agent.role.clone(),
                            source: "checkpoint".to_string(),
                            content: intent,
                            timestamp: last_checkpoint.clone().unwrap_or_default(),
                        });
                    }
                }
            }
        }

        // Parse notes
        if notes_exists {
            if let Ok(content) = fs::read_to_string(&notes_path) {
                let notes = extract_notes_entries(&content, 3);
                for note in notes {
                    all_entries.push(MemoryEntry {
                        agent: agent.name.clone(),
                        role: agent.role.clone(),
                        source: "notes".to_string(),
                        content: note,
                        timestamp: String::new(),
                    });
                }
            }
        }

        agent_summaries.push(AgentMemorySummary {
            name: agent.name.clone(),
            role: agent.role.clone(),
            memory_exists,
            checkpoint_exists,
            notes_exists,
            assignment_count,
            durable_count,
            last_checkpoint,
        });
    }

    // Sort entries by timestamp (newest first)
    all_entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    Ok(SharedMemorySnapshot {
        generated_at: inbox_timestamp_iso(),
        agents: agent_summaries,
        stats: MemoryStats {
            total_agents: agents.len(),
            agents_with_memory,
            total_entries: all_entries.len(),
            total_durable,
        },
        entries: all_entries,
    })
}

/// Format a human-readable memory summary.
pub fn format_memory_summary(snap: &SharedMemorySnapshot) -> String {
    let mut out = String::from("\n── 跨 Agent 记忆总览 ──\n\n");

    // Stats
    out.push_str(&format!(
        "  Agent 总数: {} | 有记忆: {} | 条目: {} | 持久知识: {}\n\n",
        snap.stats.total_agents,
        snap.stats.agents_with_memory,
        snap.stats.total_entries,
        snap.stats.total_durable,
    ));

    // Per-agent summary table
    out.push_str("  | Agent | 角色 | 记忆 | 检查点 | 任务 | 持久 |\n");
    out.push_str("  |-------|------|------|--------|------|------|\n");
    for a in &snap.agents {
        out.push_str(&format!(
            "  | {} | {} | {} | {} | {} | {} |\n",
            a.name,
            a.role,
            if a.memory_exists { "✓" } else { "○" },
            if a.checkpoint_exists { "✓" } else { "○" },
            a.assignment_count,
            a.durable_count,
        ));
    }

    // Durable knowledge entries
    let durable: Vec<&MemoryEntry> = snap
        .entries
        .iter()
        .filter(|e| e.source == "durable")
        .collect();
    if !durable.is_empty() {
        out.push_str("\n  ── 持久知识条目 ──\n");
        for entry in durable.iter().take(20) {
            out.push_str(&format!("  [{}] {}: {}\n", entry.agent, entry.timestamp, entry.content));
        }
    }

    // Active intents
    let intents: Vec<&MemoryEntry> = snap
        .entries
        .iter()
        .filter(|e| e.source == "checkpoint")
        .collect();
    if !intents.is_empty() {
        out.push_str("\n  ── 当前活动意图 ──\n");
        for entry in intents {
            out.push_str(&format!("  [{}] {}\n", entry.agent, entry.content));
        }
    }

    out
}

/// Build + format in one call for CLI usage.
pub fn show_memory(project_dir: &Path) -> Result<String> {
    let snap = build_shared_memory(project_dir)?;
    Ok(format_memory_summary(&snap))
}

/// Aggregate durable knowledge by category (for cross-agent sharing).
#[allow(dead_code)]
pub fn aggregate_by_category(snap: &SharedMemorySnapshot) -> BTreeMap<String, Vec<&MemoryEntry>> {
    let mut map = BTreeMap::new();
    for entry in &snap.entries {
        if entry.source != "durable" {
            continue;
        }
        // Categorize by simple keyword matching
        let category = categorize_entry(&entry.content);
        map.entry(category).or_insert_with(Vec::new).push(entry);
    }
    map
}

#[allow(dead_code)]
fn categorize_entry(content: &str) -> String {
    let lower = content.to_lowercase();
    if lower.contains("委派") || lower.contains("收到") || lower.contains("回执") {
        "delegation".to_string()
    } else if lower.contains("api") || lower.contains("后端") || lower.contains("rust") {
        "backend".to_string()
    } else if lower.contains("ui") || lower.contains("前端") || lower.contains("组件") {
        "frontend".to_string()
    } else if lower.contains("review") || lower.contains("审查") {
        "review".to_string()
    } else if lower.contains("test") || lower.contains("测试") {
        "testing".to_string()
    } else if lower.contains("架构") || lower.contains("方案") || lower.contains("adr") {
        "architecture".to_string()
    } else {
        "general".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_durable_from_template() {
        let content = r#"# Memory
Some stuff
<!-- agent-tui:durable -->
- [2024-01-15T10:30:00] 收到委派任务「auth-api」
- [2024-01-15T11:00:00] 回执「auth-api」status=done
<!-- /agent-tui:durable -->
"#;
        let entries = extract_durable_entries(content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "2024-01-15T10:30:00");
        assert!(entries[0].1.contains("auth-api"));
    }

    #[test]
    fn extract_assignment_count_from_template() {
        let content = r#"<!-- agent-tui:assignments -->
| Task | Status | Agent | Updated |
|------|--------|-------|---------|
| auth-api | done | codex | 2024-01-15 |
| login-ui | in_progress | kimi | 2024-01-15 |
<!-- /agent-tui:assignments -->
"#;
        assert_eq!(extract_assignment_count(content), 2);
    }

    #[test]
    fn categorize_by_keywords() {
        assert_eq!(categorize_entry("收到委派任务 auth-api"), "delegation");
        assert_eq!(categorize_entry("前端 UI 组件"), "frontend");
        assert_eq!(categorize_entry("后端 API 实现"), "backend");
        assert_eq!(categorize_entry("代码审查通过"), "review");
    }

    #[test]
    fn empty_content_no_entries() {
        let entries = extract_durable_entries("");
        assert!(entries.is_empty());
    }

    #[test]
    fn checkpoint_time_extraction() {
        let content = "# Session checkpoint\nUpdated: 2024-01-15T10:30:00 | Agent: codex\n";
        let time = extract_checkpoint_time(content);
        assert_eq!(time, Some("2024-01-15T10:30:00".to_string()));
    }
}
