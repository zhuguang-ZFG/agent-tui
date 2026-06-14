use std::collections::HashSet;
use std::path::Path;

use regex::Regex;
use serde::Deserialize;

use crate::delegation;
use crate::meta;
use crate::pane::AgentPane;

#[derive(Debug, Clone, Deserialize)]
pub struct ReportItem {
    pub task: String,
    pub status: String,
    #[serde(default)]
    pub summary: String,
}

pub struct ReportWatchState {
    seen_reports: HashSet<String>,
}

impl ReportWatchState {
    pub fn new() -> Self {
        Self {
            seen_reports: HashSet::new(),
        }
    }
}

fn auto_report_enabled() -> bool {
    std::env::var("AGENT_TUI_AUTO_REPORT")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn extract_agent_report_blocks(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?s)```\s*agent-report\s*\r?\n?(.*?)\r?\n?```").expect("agent-report regex")
    });
    re.captures_iter(text)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn extract_loose_report_objects(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?s)\{\s*"task"\s*:[^}]*"status"\s*:[^}]*\}"#).expect("loose report regex")
    });
    re.find_iter(text)
        .map(|m| m.as_str().trim().to_string())
        .collect()
}

pub fn collect_report_blocks(text: &str) -> Vec<String> {
    let mut blocks = extract_agent_report_blocks(text);
    for loose in extract_loose_report_objects(text) {
        if !blocks.iter().any(|b| b.contains(&loose) || loose.contains(b.as_str())) {
            blocks.push(loose);
        }
    }
    blocks
}

fn parse_report_payload(json: &str) -> Vec<ReportItem> {
    if let Ok(items) = serde_json::from_str::<Vec<ReportItem>>(json) {
        return items;
    }
    if let Ok(one) = serde_json::from_str::<ReportItem>(json) {
        return vec![one];
    }
    Vec::new()
}

fn report_fingerprint(reporter: &str, item: &ReportItem) -> String {
    format!(
        "{}:{}:{}:{}",
        reporter,
        item.task,
        item.status,
        item.summary.trim()
    )
}

fn normalize_status(status: &str) -> String {
    match status.trim().to_lowercase().as_str() {
        "done" | "complete" | "completed" | "ok" | "success" => "done".into(),
        "blocked" | "block" | "stuck" => "blocked".into(),
        "failed" | "fail" | "error" => "failed".into(),
        other if !other.is_empty() => other.into(),
        _ => "done".into(),
    }
}

/// Parse worker transcript for new agent-report items.
pub fn reports_from_text(
    reporter: &str,
    text: &str,
    seen: &mut HashSet<String>,
) -> Vec<ReportItem> {
    let mut out = Vec::new();
    for block in collect_report_blocks(text) {
        for mut item in parse_report_payload(&block) {
            if meta::validate_task_name(&item.task).is_err() {
                continue;
            }
            item.status = normalize_status(&item.status).to_string();
            let fp = report_fingerprint(reporter, &item);
            if seen.contains(&fp) {
                continue;
            }
            seen.insert(fp);
            out.push(item);
        }
    }
    out
}

/// Scan worker panes for ```agent-report``` and auto-notify the lead agent.
pub fn watch_worker_panes(
    project_dir: &Path,
    lead: &str,
    lead_index: usize,
    panes: &mut [Option<AgentPane>],
    state: &mut ReportWatchState,
) -> usize {
    if !auto_report_enabled() {
        return 0;
    }

    let mut reported = 0usize;
    for (i, pane_slot) in panes.iter().enumerate() {
        if i == lead_index {
            continue;
        }
        let Some(pane) = pane_slot.as_ref() else {
            continue;
        };
        let reporter = pane.spec.name.clone();
        if reporter.eq_ignore_ascii_case(lead) {
            continue;
        }
        let text = pane.transcript_text();
        let items = reports_from_text(&reporter, &text, &mut state.seen_reports);
        for item in items {
            match delegation::report_task_auto(
                project_dir,
                &reporter,
                lead,
                &item.task,
                &item.status,
                &item.summary,
            ) {
                Ok(()) => reported += 1,
                Err(e) => {
                    crate::terminal::log_message(
                        project_dir,
                        "warn",
                        &format!(
                            "auto-report {}/{} failed: {e:#}",
                            reporter, item.task
                        ),
                    );
                }
            }
        }
    }
    reported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_report_block() {
        let text = r#"
```agent-report
{"task":"auth-api","status":"done","summary":"login ok"}
```
"#;
        let mut seen = HashSet::new();
        let items = reports_from_text("codex", text, &mut seen);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].task, "auth-api");
        assert_eq!(items[0].status, "done");
    }

    #[test]
    fn dedupes_same_report() {
        let text = r#"
```agent-report
{"task":"auth-api","status":"done","summary":"ok"}
```
```agent-report
{"task":"auth-api","status":"done","summary":"ok"}
```
"#;
        let mut seen = HashSet::new();
        let items = reports_from_text("codex", text, &mut seen);
        assert_eq!(items.len(), 1);
    }
}
