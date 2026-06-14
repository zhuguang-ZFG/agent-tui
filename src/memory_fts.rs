//! SQLite FTS5 index over per-agent memory files (MEMORY / checkpoint / notes).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::agent_memory::{checkpoint_path, memory_md_path, notes_path};
use crate::config;

fn db_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents/shared/memory_fts.db")
}

fn open_db(project_dir: &Path) -> Result<Connection> {
    let path = db_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
            agent UNINDEXED,
            source UNINDEXED,
            path UNINDEXED,
            body,
            tokenize='unicode61 remove_diacritics 0'
        );
        "#,
    )?;
    Ok(conn)
}

#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub agent: String,
    pub source: String,
    pub path: String,
    pub snippet: String,
}

fn sources_for_agent(project_dir: &Path, agent: &str) -> [(&'static str, PathBuf); 3] {
    [
        ("MEMORY", memory_md_path(project_dir, agent)),
        ("checkpoint", checkpoint_path(project_dir, agent)),
        ("notes", notes_path(project_dir, agent)),
    ]
}

/// Rebuild FTS rows for one agent.
pub fn index_agent(project_dir: &Path, agent: &str) -> Result<()> {
    let conn = open_db(project_dir)?;
    conn.execute(
        "DELETE FROM memory_fts WHERE agent = ?1",
        params![agent],
    )?;
    for (source, path) in sources_for_agent(project_dir, agent) {
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        if body.trim().is_empty() {
            continue;
        }
        conn.execute(
            "INSERT INTO memory_fts(agent, source, path, body) VALUES (?1, ?2, ?3, ?4)",
            params![agent, source, path.to_string_lossy().as_ref(), body],
        )?;
    }
    Ok(())
}

/// Rebuild the full memory FTS index for all configured agents.
pub fn reindex_all(project_dir: &Path) -> Result<usize> {
    let agents = config::load_agents(project_dir)?;
    let conn = open_db(project_dir)?;
    conn.execute("DELETE FROM memory_fts", [])?;
    let mut n = 0usize;
    for spec in &agents {
        for (source, path) in sources_for_agent(project_dir, &spec.name) {
            let Ok(body) = fs::read_to_string(&path) else {
                continue;
            };
            if body.trim().is_empty() {
                continue;
            }
            conn.execute(
                "INSERT INTO memory_fts(agent, source, path, body) VALUES (?1, ?2, ?3, ?4)",
                params![
                    spec.name,
                    source,
                    path.to_string_lossy().as_ref(),
                    body
                ],
            )?;
            n += 1;
        }
    }
    Ok(n)
}

fn build_match_query(raw: &str) -> Option<String> {
    let terms: Vec<String> = raw
        .split_whitespace()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| {
            let escaped = t.replace('"', "");
            format!("\"{escaped}\"*")
        })
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" AND "))
    }
}

/// Search indexed memory bodies; returns empty if query is blank or index missing.
pub fn search(project_dir: &Path, query: &str, limit: usize) -> Result<Vec<MemoryHit>> {
    let Some(match_query) = build_match_query(query) else {
        return Ok(Vec::new());
    };
    if !db_path(project_dir).is_file() {
        reindex_all(project_dir)?;
    }
    let conn = open_db(project_dir)?;
    let limit = limit.clamp(1, 50) as i64;
    let mut stmt = conn.prepare(
        r#"
        SELECT agent, source, path,
               snippet(memory_fts, 3, '[', ']', '…', 16) AS snip
        FROM memory_fts
        WHERE body MATCH ?1
        ORDER BY rank
        LIMIT ?2
        "#,
    )?;
    let rows = stmt.query_map(params![match_query, limit], |row| {
        Ok(MemoryHit {
            agent: row.get(0)?,
            source: row.get(1)?,
            path: row.get(2)?,
            snippet: row.get(3)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().context("collect fts hits")
}

pub fn format_hits(hits: &[MemoryHit]) -> Vec<String> {
    if hits.is_empty() {
        return vec!["（无匹配）".into()];
    }
    hits.iter()
        .map(|h| {
            format!(
                "[{}·{}] {}",
                h.agent,
                h.source,
                h.snippet.replace('\n', " ")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_memory;

    #[test]
    fn index_and_search_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "agent-tui-fts-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".agents/codex/memory")).unwrap();
        fs::write(
            dir.join(".agents/agents.yaml"),
            r#"
agents:
  codex:
    command: cmd.exe
    role: executor
    enabled: true
"#,
        )
        .unwrap();
        let notes = dir.join(".agents/codex/memory/notes.md");
        fs::write(&notes, "- [test] unique-keyword-fts-alpha\n").unwrap();
        agent_memory::ensure_agent_memory_files(&dir, "codex", "executor", "cursor").unwrap();

        reindex_all(&dir).unwrap();
        let hits = search(&dir, "unique-keyword-fts-alpha", 5).unwrap();
        assert!(!hits.is_empty(), "expected fts hit");
        assert!(hits[0].agent.eq_ignore_ascii_case("codex"));

        let _ = fs::remove_dir_all(&dir);
    }
}
