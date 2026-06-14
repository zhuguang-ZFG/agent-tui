//! Hard review gate: implementation `done` requires `{task}-review` to pass before merge/DAG complete.

use std::path::Path;

use crate::config;
use crate::task_dag;
use crate::task_state;
use crate::terminal;

pub struct ReviewGateOutcome {
    pub status: String,
    pub summary: String,
    /// When set, caller should `delegate_task(lead, reviewer, review_id, desc)`.
    pub delegate_review: Option<(String, String, String)>,
}

pub fn review_gate_enabled() -> bool {
    std::env::var("AGENT_TUI_REVIEW_GATE")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

pub fn review_suffix() -> &'static str {
    "-review"
}

pub fn is_review_task(task: &str) -> bool {
    task.ends_with(review_suffix()) && task.len() > review_suffix().len()
}

pub fn review_task_id(task: &str) -> String {
    format!("{task}{}", review_suffix())
}

pub fn parent_task(review_task: &str) -> Option<String> {
    if !is_review_task(review_task) {
        return None;
    }
    Some(
        review_task
            .strip_suffix(review_suffix())?
            .to_string(),
    )
}

pub fn pick_reviewer(project_dir: &Path, lead: &str) -> Option<String> {
    config::load_agents(project_dir)
        .ok()?
        .into_iter()
        .find(|a| a.role == "reviewer" && !a.name.eq_ignore_ascii_case(lead))
        .map(|a| a.name)
        .or_else(|| {
            config::load_agents(project_dir)
                .ok()?
                .into_iter()
                .find(|a| a.role == "advisor" && !a.name.eq_ignore_ascii_case(lead))
                .map(|a| a.name)
        })
}

fn review_already_done(project_dir: &Path, impl_task: &str) -> bool {
    let review_id = review_task_id(impl_task);
    task_state::load_snapshots(project_dir)
        .get(&review_id)
        .is_some_and(|s| s.status == "done")
}

fn review_in_flight(project_dir: &Path, impl_task: &str) -> bool {
    let review_id = review_task_id(impl_task);
    task_state::load_snapshots(project_dir)
        .get(&review_id)
        .is_some_and(|s| {
            matches!(
                s.status.as_str(),
                "delegated" | "pending" | "awaiting_review" | "done"
            )
        })
}

/// After implementer reports `done`, require review task unless already reviewed.
pub fn apply_implementation_done(
    project_dir: &Path,
    lead: &str,
    reporter: &str,
    task: &str,
    summary: &str,
) -> ReviewGateOutcome {
    if !review_gate_enabled() || is_review_task(task) {
        return ReviewGateOutcome {
            status: "done".into(),
            summary: summary.to_string(),
            delegate_review: None,
        };
    }
    if review_already_done(project_dir, task) {
        return ReviewGateOutcome {
            status: "done".into(),
            summary: summary.to_string(),
            delegate_review: None,
        };
    }

    let review_id = review_task_id(task);
    let delegate_review = if !review_in_flight(project_dir, task) {
        pick_reviewer(project_dir, lead).map(|reviewer| {
            let desc = format!(
                "审查 {reporter}/{task} 产出。通过则 agent-report done；不通过则 failed 并说明原因。"
            );
            (reviewer, review_id.clone(), desc)
        })
    } else {
        None
    };

    if delegate_review.is_none() && !review_in_flight(project_dir, task) {
        terminal::log_message(
            project_dir,
            "warn",
            &format!("review gate: 无 reviewer Agent，{task} 直接 done"),
        );
        return ReviewGateOutcome {
            status: "done".into(),
            summary: summary.to_string(),
            delegate_review: None,
        };
    }

    ReviewGateOutcome {
        status: "awaiting_review".into(),
        summary: format!(
            "{summary}（待审查 {review_id}，review 通过后计入 done / merge-ready）"
        ),
        delegate_review,
    }
}

/// Review task finished — promote parent implementation task to `done`. Returns parent id if promoted.
pub fn promote_parent_after_review(
    project_dir: &Path,
    lead: &str,
    reviewer: &str,
    review_task: &str,
    review_status: &str,
    summary: &str,
) -> Option<String> {
    let parent = parent_task(review_task)?;
    if review_status == "done" {
        let _ = task_state::on_report(
            project_dir,
            reviewer,
            lead,
            &parent,
            "done",
            &format!("review 通过: {summary}"),
        );
        let _ = task_dag::mark_task_completed(project_dir, &parent);
        terminal::log_message(
            project_dir,
            "info",
            &format!("review gate: {parent} 审查通过 → done"),
        );
        Some(parent)
    } else if review_status == "failed" {
        let _ = task_state::on_report(
            project_dir,
            reviewer,
            lead,
            &parent,
            "review_failed",
            &format!("review 未通过: {summary}"),
        );
        terminal::log_message(
            project_dir,
            "warn",
            &format!("review gate: {parent} 审查未通过"),
        );
        None
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_task_naming() {
        assert!(is_review_task("auth-api-review"));
        assert!(!is_review_task("auth-api"));
        assert_eq!(
            parent_task("auth-api-review").as_deref(),
            Some("auth-api")
        );
        assert_eq!(review_task_id("auth-api"), "auth-api-review");
    }
}
