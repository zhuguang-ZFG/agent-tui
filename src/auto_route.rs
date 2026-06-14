//! `agent-tui gen-routes` — scan project structure and auto-generate routing.yaml.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::config::{load_agents, AgentSpec};

/// Directory pattern → (role_hint, keywords)
const DIR_PATTERNS: &[(&[&str], &str, &[&str])] = &[
    // Frontend / UI
    (&["src/ui", "src/components", "src/views", "src/pages", "src/widgets", "app/ui", "components", "pages"],
     "frontend",
     &["ui", "react", "component", "vue", "svelte", "tailwind", "css", "frontend", "页面", "组件"]),
    // Backend / API
    (&["src/api", "src/server", "src/backend", "src/routes", "src/handlers", "src/controllers", "app/api"],
     "executor",
     &["api", "server", "route", "handler", "endpoint", "backend", "rest", "graphql", "后端"]),
    // Database / Data layer
    (&["src/db", "src/models", "src/schema", "src/repository", "migrations", "src/data"],
     "executor",
     &["db", "sql", "model", "schema", "migration", "database", "orm", "repository", "数据"]),
    // Tests
    (&["tests", "test", "spec", "__tests__", "e2e", "integration", "benchmarks", "benches"],
     "tester",
     &["test", "spec", "e2e", "pytest", "vitest", "jest", "benchmark", "测试", "qa"]),
    // CI/CD / DevOps
    (&[".github", ".gitlab-ci", "docker", "k8s", "infra", "scripts/deploy", ".circleci"],
     "integrator",
     &["ci", "cd", "docker", "deploy", "pipeline", "kubernetes", "helm", "terraform", "部署"]),
    // Config / Scripts
    (&["config", "scripts", "tools", "bin", "Makefile", "justfile", "Taskfile"],
     "integrator",
     &["config", "script", "tool", "build", "task", "makefile", "配置", "脚本"]),
    // Documentation
    (&["docs", "doc", "wiki", "adr", "rfc", "design"],
     "analyst",
     &["doc", "readme", "adr", "rfc", "design", "guide", "文档", "方案"]),
    // CLI / Commands
    (&["src/cli", "src/cmd", "src/bin", "src/main"],
     "executor",
     &["cli", "command", "arg", "flag", "subcommand", "命令行"]),
    // Core / Lib (general backend)
    (&["src/lib", "src/core", "src/common", "src/utils", "src/shared"],
     "executor",
     &["lib", "core", "util", "common", "shared", "helper", "核心", "重构"]),
];

/// File extension → keyword hints
const EXT_PATTERNS: &[(&[&str], &str, &[&str])] = &[
    (&[".rs"], "executor", &["rust", "cargo", "crate"]),
    (&[".ts", ".tsx", ".jsx", ".js"], "frontend", &["typescript", "javascript", "react", "node"]),
    (&[".py"], "executor", &["python", "pip", "django", "flask", "fastapi"]),
    (&[".go"], "executor", &["golang", "go"]),
    (&[".java", ".kt"], "executor", &["java", "kotlin", "spring", "jvm"]),
    (&[".css", ".scss", ".sass", ".less"], "frontend", &["css", "style", "scss", "tailwind"]),
    (&[".yaml", ".yml", ".toml", ".json"], "integrator", &["config", "yaml", "toml", "json"]),
    (&[".sh", ".bash", ".zsh", ".ps1"], "integrator", &["shell", "bash", "script", "powershell"]),
    (&[".md"], "analyst", &["doc", "markdown", "readme"]),
    (&[".sql"], "executor", &["sql", "query", "migration"]),
    (&[".dockerfile", "Dockerfile"], "integrator", &["docker", "container"]),
];

/// Scan a project and produce auto-generated routing rules.
pub fn scan_and_generate(project_dir: &Path) -> Result<AutoRouteOutput> {
    let agents = load_agents(project_dir)?;
    let mut role_keywords: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut role_dirs: BTreeMap<String, Vec<String>> = BTreeMap::new();

    // Scan directory structure
    scan_dirs(project_dir, project_dir, &mut role_keywords, &mut role_dirs, 0)?;

    // Scan file extensions
    scan_extensions(project_dir, project_dir, &mut role_keywords, 0)?;

    // Build routing rules
    let mut rules = Vec::new();
    let mut covered_roles: BTreeSet<String> = BTreeSet::new();
    for (role, keywords) in &role_keywords {
        // Find agents matching this role
        let matching_agents: Vec<&AgentSpec> = agents
            .iter()
            .filter(|a| a.role.eq_ignore_ascii_case(role))
            .collect();

        for agent in matching_agents {
            let kw_vec: Vec<String> = keywords.iter().cloned().collect();
            if !kw_vec.is_empty() {
                covered_roles.insert(role.clone());
                rules.push(RoutingRule {
                    match_keywords: kw_vec,
                    worker: agent.name.clone(),
                    role: role.clone(),
                });
            }
        }
    }

    // Fallback: add generic keywords for agents whose roles weren't detected
    for agent in &agents {
        if !covered_roles.iter().any(|r| r.eq_ignore_ascii_case(&agent.role)) {
            let kws = role_to_generic_keywords(&agent.role);
            if !kws.is_empty() {
                covered_roles.insert(agent.role.clone());
                rules.push(RoutingRule {
                    match_keywords: kws,
                    worker: agent.name.clone(),
                    role: agent.role.clone(),
                });
            }
        }
    }

    Ok(AutoRouteOutput {
        rules,
        role_dirs,
        agents_scanned: agents.len(),
    })
}

fn scan_dirs(
    root: &Path,
    dir: &Path,
    role_keywords: &mut BTreeMap<String, BTreeSet<String>>,
    role_dirs: &mut BTreeMap<String, Vec<String>>,
    depth: usize,
) -> Result<()> {
    if depth > 4 {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    let rel = dir.strip_prefix(root).unwrap_or(dir);
    let rel_str = rel.to_string_lossy().replace('\\', "/");

    // Check if this directory matches any known pattern
    for (patterns, role, keywords) in DIR_PATTERNS {
        if patterns.iter().any(|p| rel_str.ends_with(p) || rel_str.contains(p)) {
            let entry = role_keywords.entry(role.to_string()).or_default();
            for kw in *keywords {
                entry.insert(kw.to_string());
            }
            role_dirs
                .entry(role.to_string())
                .or_default()
                .push(rel_str.clone());
        }
    }

    // Also extract directory name as keyword — only for specific known module names
    if let Some(name) = dir.file_name() {
        let name = name.to_string_lossy().to_lowercase();
        // Only add directory name as keyword if it matches known domain terms
        const DOMAIN_DIRS: &[(&str, &str)] = &[
            ("auth", "executor"), ("payment", "executor"), ("billing", "executor"),
            ("notification", "executor"), ("email", "executor"), ("search", "executor"),
            ("analytics", "analyst"), ("logging", "executor"), ("cache", "executor"),
            ("queue", "executor"), ("worker", "executor"), ("scheduler", "executor"),
        ];
        for (dir_name, role) in DOMAIN_DIRS {
            if name == *dir_name {
                let entry = role_keywords.entry(role.to_string()).or_default();
                entry.insert(name.clone());
            }
        }
    }

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !is_skip_dir(&name) {
                scan_dirs(root, &path, role_keywords, role_dirs, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn scan_extensions(
    _root: &Path,
    dir: &Path,
    role_keywords: &mut BTreeMap<String, BTreeSet<String>>,
    depth: usize,
) -> Result<()> {
    if depth > 3 {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !is_skip_dir(&name) {
                scan_extensions(_root, &path, role_keywords, depth + 1)?;
            }
        } else if path.is_file() {
            if let Some(ext) = path.extension() {
                let ext_str = format!(".{}", ext.to_string_lossy().to_lowercase());
                for (exts, role, keywords) in EXT_PATTERNS {
                    if exts.iter().any(|e| e == &ext_str) {
                        let entry = role_keywords.entry(role.to_string()).or_default();
                        for kw in *keywords {
                            entry.insert(kw.to_string());
                        }
                    }
                }
            }
            // Check filename for Dockerfile etc
            let fname = entry.file_name().to_string_lossy().to_lowercase();
            if fname == "dockerfile" || fname == "docker-compose.yml" {
                let entry = role_keywords.entry("integrator".to_string()).or_default();
                entry.insert("docker".to_string());
                entry.insert("container".to_string());
            }
        }
    }
    Ok(())
}

fn is_skip_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | "target"
            | ".git"
            | ".agents"
            | "vendor"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | "__pycache__"
            | ".cache"
            | ".omx"
            | ".omc"
            | ".omk"
    )
}

fn role_to_generic_keywords(role: &str) -> Vec<String> {
    match role {
        "frontend" => vec!["ui", "react", "component", "css", "frontend", "页面", "组件"],
        "executor" => vec!["api", "backend", "rust", "python", "逻辑", "重构", "后端"],
        "reviewer" => vec!["review", "审查", "质量", "smoke", "review"],
        "tester" => vec!["test", "测试", "e2e", "pytest", "benchmark", "qa"],
        "integrator" => vec!["ci", "cd", "docker", "deploy", "pipeline", "部署", "集成"],
        "analyst" => vec!["分析", "文档", "索引", "架构", "方案", "reasoning"],
        "advisor" => vec!["架构", "adr", "blocked", "根因", "方案"],
        _ => Vec::new(),
    }
    .into_iter()
    .map(String::from)
    .collect()
}

#[derive(Debug)]
pub struct RoutingRule {
    pub match_keywords: Vec<String>,
    pub worker: String,
    #[allow(dead_code)]
    pub role: String,
}

pub struct AutoRouteOutput {
    pub rules: Vec<RoutingRule>,
    pub role_dirs: BTreeMap<String, Vec<String>>,
    pub agents_scanned: usize,
}

/// Format the auto-generated routing as YAML.
pub fn format_routing_yaml(output: &AutoRouteOutput) -> String {
    let mut yaml = String::from(
        "# Auto-generated by `agent-tui gen-routes`\n\
         # match keywords extracted from project structure + file extensions.\n\
         # Edit freely — this file is a starting template.\n\
         rules:\n",
    );
    for rule in &output.rules {
        let kws: Vec<String> = rule.match_keywords.iter().map(|k| format!("\"{}\"", k)).collect();
        yaml.push_str(&format!(
            "  - match: [{}]\n    worker: {}\n",
            kws.join(", "),
            rule.worker
        ));
    }
    yaml
}

/// Format a human-readable summary of the scan.
pub fn format_scan_summary(output: &AutoRouteOutput) -> String {
    let mut out = String::from("\n── 代码图谱扫描 ──\n");
    out.push_str(&format!("  Agent 数量: {}\n", output.agents_scanned));
    for (role, dirs) in &output.role_dirs {
        out.push_str(&format!("  {role}: {}\n", dirs.join(", ")));
    }
    out.push_str(&format!("  生成路由规则: {} 条\n", output.rules.len()));
    out
}

/// Scan + generate + write routing.yaml (with --dry-run support).
pub fn gen_routes(project_dir: &Path, dry_run: bool) -> Result<String> {
    let output = scan_and_generate(project_dir)?;
    let yaml = format_routing_yaml(&output);
    let summary = format_scan_summary(&output);

    let mut log = summary;
    if dry_run {
        log.push_str("\n── Dry Run: routing.yaml 内容 ──\n");
        log.push_str(&yaml);
    } else {
        let path = project_dir.join(".agents/routing.yaml");
        // Backup existing
        if path.is_file() {
            let backup = path.with_extension("yaml.bak");
            fs::copy(&path, &backup)
                .with_context(|| format!("backup {}", backup.display()))?;
            log.push_str(&format!("  已备份 → {}\n", backup.display()));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, &yaml)?;
        log.push_str(&format!("  ✓ 已写入 {}\n", path.display()));
    }
    Ok(log)
}
