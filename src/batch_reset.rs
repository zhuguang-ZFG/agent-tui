//! Reset delivery-batch fingerprints for a new sprint (keeps task history).

use std::fs;
use std::path::Path;

use anyhow::Result;

#[derive(Debug, Clone, Default)]
pub struct ResetBatchReport {
    pub removed_files: Vec<String>,
}

const FINGERPRINT_FILES: &[&str] = &[
    "merge_ready_notified.jsonl",
    "post_merge_smoke_dispatched.jsonl",
    "auto_pr_dispatched.jsonl",
    "auto_merge_dispatched.jsonl",
    "post_github_merge_dispatched.jsonl",
    "pr_merged_notified.jsonl",
    "batch_review_dispatched.jsonl",
    "batch_review_failed_notified.jsonl",
    "pr_merged_simulate.json",
    "pr_ci_pass_simulate.json",
];

pub fn reset_batch_fingerprints(project_dir: &Path) -> Result<ResetBatchReport> {
    let shared = project_dir.join(".agents/shared");
    let mut report = ResetBatchReport::default();
    for name in FINGERPRINT_FILES {
        let path = shared.join(name);
        if path.is_file() {
            fs::remove_file(&path)?;
            report.removed_files.push(name.to_string());
        }
    }
    Ok(report)
}

pub fn format_reset_report(report: &ResetBatchReport) -> String {
    if report.removed_files.is_empty() {
        return "交付批次指纹已是干净状态（无文件需删）。".into();
    }
    format!(
        "已重置交付批次指纹（{} 个文件），可开始新 sprint。",
        report.removed_files.len()
    )
}
