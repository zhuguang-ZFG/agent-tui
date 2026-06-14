//! TUI / 留言板可执行的运维与交付操作（与子命令共用逻辑）。

use std::path::Path;

use anyhow::{bail, Result};

use crate::batch_reset;
use crate::batch_review;
use crate::delegation_stats;
use crate::lead_identity;
use crate::merge;
use crate::pr_create;
use crate::pr_lifecycle;
use crate::project_init;
use crate::project_map;
use crate::verify_cleanup;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsVerb {
    ShowGuide,
    ShowNext,
    PrCreate,
    PrMerge { squash: bool },
    PrStatus,
    Merge { all: bool },
    Review { force: bool },
    CleanVerify,
    ResetBatch,
    SyncLead,
    Doctor,
    Evolve,
    ProjectMap,
}

/// Inbox `!` command catalog (prefix, one-line description).
pub const OPS_CATALOG: &[(&str, &str)] = &[
    ("!任务", "交给 Lead 拆解（自然语言）"),
    ("!map", "生成 PROJECT_MAP.md"),
    ("!next", "当前阶段与下一步"),
    ("!guide", "速查表"),
    ("!doctor", "项目体检与能力清单"),
    ("!pr", "开 GitHub PR（需 gh）"),
    ("!pr-merge", "合并当前 PR"),
    ("!pr-status", "查询 PR 状态"),
    ("!merge", "合并 agent 分支"),
    ("!merge-all", "合并全部分支"),
    ("!review", "批次代码审查"),
    ("!sync-lead", "刷新 Lead 规则"),
    ("!evolve", "更新 STRENGTHS 历史"),
    ("!clean", "清理 verify-loop 残留"),
    ("!reset-batch", "重置交付批次指纹"),
];

/// Hint line while typing `!…` in inbox.
pub fn inbox_hint(input: &str) -> Option<String> {
    let t = input.trim();
    if t.is_empty() || t == "!" {
        return Some("!任务 !map !doctor !guide !next !pr …".into());
    }
    if !t.starts_with('!') {
        if t.starts_with('@') {
            return Some("定向通知：@mimo 消息正文".into());
        }
        if t.starts_with("!任务") || t.contains(' ') {
            return None;
        }
        return Some("任务：!任务 描述需求  |  运维：! 开头见补全".into());
    }
    let exact = OPS_CATALOG
        .iter()
        .find(|(cmd, _)| *cmd == t)
        .map(|(_, desc)| (*desc).to_string());
    if let Some(desc) = exact {
        return Some(desc);
    }
    let matches: Vec<&str> = OPS_CATALOG
        .iter()
        .filter(|(cmd, _)| cmd.starts_with(t))
        .map(|(cmd, _)| *cmd)
        .collect();
    if matches.is_empty() {
        return Some("未知命令 — 输入 !guide 查看速查".into());
    }
    Some(matches.into_iter().take(6).collect::<Vec<_>>().join("  "))
}

/// Parse `!pr`, `!merge-all`, etc. from inbox input (must be trimmed, non-empty).
pub fn parse_ops_line(raw: &str) -> Option<OpsVerb> {
    let trimmed = raw.trim();
    let (head, rest) = trimmed.split_once(char::is_whitespace).map(|(h, r)| (h, r.trim())).unwrap_or((trimmed, ""));

    match head {
        "!guide" | "!速查" | "!帮助" => Some(OpsVerb::ShowGuide),
        "!next" | "!进度" | "!阶段" => Some(OpsVerb::ShowNext),
        "!pr" | "!pr-create" | "!开pr" => Some(OpsVerb::PrCreate),
        "!pr-merge" | "!合并pr" => Some(OpsVerb::PrMerge {
            squash: rest.eq_ignore_ascii_case("--squash") || rest.contains("squash"),
        }),
        "!pr-status" | "!pr状态" => Some(OpsVerb::PrStatus),
        "!merge-all" | "!合并全部" => Some(OpsVerb::Merge { all: true }),
        "!merge" | "!合并" => Some(OpsVerb::Merge {
            all: rest.eq_ignore_ascii_case("--all") || rest.contains("全部"),
        }),
        "!review" | "!重审" | "!批次审查" => Some(OpsVerb::Review {
            force: rest.eq_ignore_ascii_case("--force") || rest.contains("force"),
        }),
        "!clean" | "!clean-verify" | "!清理" => Some(OpsVerb::CleanVerify),
        "!reset-batch" | "!新批次" | "!新sprint" => Some(OpsVerb::ResetBatch),
        "!sync-lead" | "!同步lead" => Some(OpsVerb::SyncLead),
        "!doctor" | "!体检" => Some(OpsVerb::Doctor),
        "!evolve" | "!进化" => Some(OpsVerb::Evolve),
        "!map" | "!project-map" | "!项目地图" => Some(OpsVerb::ProjectMap),
        _ => None,
    }
}

pub fn is_show_panel(verb: &OpsVerb) -> bool {
    matches!(verb, OpsVerb::ShowGuide | OpsVerb::ShowNext)
}

pub fn requires_confirmation(verb: &OpsVerb) -> bool {
    matches!(
        verb,
        OpsVerb::PrCreate
            | OpsVerb::PrMerge { .. }
            | OpsVerb::Merge { all: true }
            | OpsVerb::Review { force: true }
            | OpsVerb::ResetBatch
    )
}

pub fn describe_confirm(verb: &OpsVerb) -> String {
    match verb {
        OpsVerb::PrCreate => {
            "⚠ 将执行 gh pr create（开 Pull Request）\n\n确认？此操作会影响远程仓库。".into()
        }
        OpsVerb::PrMerge { squash } => format!(
            "⚠ 将执行 gh pr merge{}（合并当前 PR）\n\n确认？合并后通常不可轻易撤销。",
            if *squash { " --squash" } else { "" }
        ),
        OpsVerb::Merge { all: true } => {
            "⚠ 将执行 merge --all（非交互合并所有 agent 分支）\n\n确认？请确保各 worktree 已提交。".into()
        }
        OpsVerb::Review { force: true } => {
            "⚠ 将强制重置批次审查指纹并重新委派 review\n\n确认？".into()
        }
        OpsVerb::ResetBatch => {
            "⚠ 将重置交付批次指纹（merge-ready / auto-pr 等去重状态）\n\n确认？不影响 task 历史。".into()
        }
        _ => format!("确认执行 {verb:?}？"),
    }
}

pub fn execute(project_dir: &Path, lead: &str, verb: OpsVerb) -> Result<String> {
    match verb {
        OpsVerb::ShowGuide | OpsVerb::ShowNext => Ok(String::new()),
        OpsVerb::PrCreate => {
            let outcome = pr_create::create_pr(project_dir, None, None, None, false)?;
            let mut msg = outcome.message;
            if let Some(url) = outcome.url {
                msg.push_str(&format!("\n{url}"));
            }
            if !outcome.created {
                anyhow::bail!(msg);
            }
            Ok(msg)
        }
        OpsVerb::PrMerge { squash } => {
            let msg = pr_lifecycle::merge_pr(project_dir, squash)?;
            Ok(format!("{msg}\nTUI 将轮询 PR 合并并派 post-github-merge 验证"))
        }
        OpsVerb::PrStatus => {
            let report = pr_lifecycle::query_pr_status(project_dir)?;
            let mut lines = vec![
                format!("分支: {}", report.branch),
                format!("状态: {:?}", report.state),
                report.message,
            ];
            if let Some(url) = report.url {
                lines.push(format!("URL: {url}"));
            }
            Ok(lines.join("\n"))
        }
        OpsVerb::Merge { all } => {
            if !all {
                bail!(
                    "交互式 !merge 会阻塞 TUI。请用 !merge-all，或在另一终端运行: agent-tui merge --project-dir …"
                );
            }
            let outcome = merge::run_merge(project_dir, all, None)?;
            let mut lines = vec![outcome.message];
            if let Some(branch) = outcome.merge_branch {
                lines.push(format!("合并分支: {branch}"));
            }
            if !outcome.merged_agents.is_empty() {
                lines.push(format!("已合并: {}", outcome.merged_agents.join(", ")));
            }
            Ok(lines.join("\n"))
        }
        OpsVerb::Review { force } => {
            if force {
                batch_review::reset_dispatched(project_dir)?;
            }
            let task_id = batch_review::dispatch_review_for_project(project_dir, None)?;
            Ok(format!("已委派批次代码审查：{task_id}"))
        }
        OpsVerb::CleanVerify => {
            let report = verify_cleanup::cleanup_verify_artifacts(project_dir)?;
            Ok(verify_cleanup::format_cleanup_report(&report))
        }
        OpsVerb::ResetBatch => {
            let report = batch_reset::reset_batch_fingerprints(project_dir)?;
            Ok(batch_reset::format_reset_report(&report))
        }
        OpsVerb::SyncLead => {
            let agents = crate::config::load_agents(project_dir)?;
            let spec = agents
                .iter()
                .find(|a| a.name.eq_ignore_ascii_case(lead))
                .ok_or_else(|| anyhow::anyhow!("Lead {lead} 不在 agents.yaml"))?;
            lead_identity::sync_lead_context(project_dir, lead, &spec.worktree)?;
            let _ = crate::agent_memory::refresh_lead_identity(project_dir, lead);
            crate::coord_dedupe::clear_initial_briefing(project_dir);
            Ok(format!(
                "已同步 Lead 规则 → {}\n（已清除 briefing 缓存；F5 重启 Lead 格子或重开 TUI 将重新注入）",
                spec.worktree
                    .join(".cursor/rules/agent-tui-orchestrator.mdc")
                    .display()
            ))
        }
        OpsVerb::Doctor => {
            let status = project_init::detect_project_or_cwd(Some(project_dir.to_path_buf()));
            Ok(project_init::format_doctor_report(&status))
        }
        OpsVerb::Evolve => {
            let n = delegation_stats::evolve_project(project_dir)?;
            Ok(format!("已更新 STRENGTHS 历史表现（{n} 条委派记录）"))
        }
        OpsVerb::ProjectMap => project_map::generate(project_dir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ops_verbs() {
        assert_eq!(parse_ops_line("!pr"), Some(OpsVerb::PrCreate));
        assert_eq!(parse_ops_line("!map"), Some(OpsVerb::ProjectMap));
        assert_eq!(
            parse_ops_line("!merge --all"),
            Some(OpsVerb::Merge { all: true })
        );
        assert_eq!(
            parse_ops_line("!review --force"),
            Some(OpsVerb::Review { force: true })
        );
        assert_eq!(parse_ops_line("!任务 x"), None);
    }

    #[test]
    fn confirm_gate() {
        assert!(requires_confirmation(&OpsVerb::PrCreate));
        assert!(requires_confirmation(&OpsVerb::Merge { all: true }));
        assert!(!requires_confirmation(&OpsVerb::PrStatus));
        assert!(!requires_confirmation(&OpsVerb::Merge { all: false }));
    }

    #[test]
    fn inbox_hint_suggests_prefix() {
        let h = inbox_hint("!pr").unwrap();
        assert!(h.contains("PR") || h.contains("pr"));
        let partial = inbox_hint("!m").unwrap();
        assert!(partial.contains("!map") || partial.contains("!merge"));
    }

    #[test]
    fn inbox_merge_noninteractive_only() {
        let dir = std::env::temp_dir().join(format!("ops-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".agents/shared")).unwrap();
        std::fs::write(
            dir.join(".agents/agents.yaml"),
            "agents:\n  claude:\n    command: claude\n    role: architect\n    enabled: true\n",
        )
        .unwrap();
        let err = execute(&dir, "claude", OpsVerb::Merge { all: false }).unwrap_err();
        assert!(err.to_string().contains("阻塞 TUI"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
