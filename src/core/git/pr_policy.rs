use serde::{Deserialize, Serialize};
use std::fs;

use crate::core::error::{Error, Result};

use super::changes::get_dirty_files;
use super::github::{pr_files, pr_merge, pr_view, PrMergeOptions};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PrPolicyFile {
    #[serde(default)]
    pub open: PrPolicyRules,
    #[serde(default)]
    pub merge: PrPolicyRules,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PrPolicyRules {
    #[serde(default)]
    pub allowed_sources: Vec<String>,
    #[serde(default)]
    pub allowed_authors: Vec<String>,
    #[serde(default)]
    pub allowed_base_branches: Vec<String>,
    #[serde(default)]
    pub allowed_head_branches: Vec<String>,
    #[serde(default)]
    pub allowed_head_repositories: Vec<String>,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub blocked_paths: Vec<String>,
    #[serde(default)]
    pub blocked_content_patterns: Vec<String>,
    #[serde(default)]
    pub allowed_merge_methods: Vec<String>,
    #[serde(default)]
    pub require_same_repository: Option<bool>,
    pub max_changed_files: Option<usize>,
    #[serde(default)]
    pub delete_branch_on_merge: Option<bool>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Shared PR target references (`base`, `head`, `head_repository`,
/// `repository`) carried by the policy context and the open/merge options.
#[derive(Debug, Clone, Default)]
pub struct PrPolicyTargetRefs {
    pub base: Option<String>,
    pub head: Option<String>,
    pub head_repository: Option<String>,
    pub repository: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PrPolicyContext {
    pub mode: PrPolicyMode,
    pub source: Option<String>,
    pub author: Option<String>,
    pub refs: PrPolicyTargetRefs,
    pub merge_method: Option<String>,
    pub files: Vec<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PrPolicyMode {
    #[default]
    Open,
    Merge,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrPolicyDecision {
    pub mode: String,
    pub allowed: bool,
    pub safe: bool,
    pub reason: String,
    pub report: String,
    pub changed_file_count: usize,
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_next_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct PrPolicyOpenOptions {
    pub component_id: String,
    pub path: Option<String>,
    pub policy_path: String,
    pub source: Option<String>,
    pub refs: PrPolicyTargetRefs,
    pub files: Vec<String>,
    pub files_from_git: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PrPolicyMergeOptions {
    pub component_id: String,
    pub path: Option<String>,
    pub policy_path: String,
    pub number: u64,
    pub author: Option<String>,
    pub refs: PrPolicyTargetRefs,
    pub merge: bool,
    pub merge_method: Option<String>,
}

fn load_policy(path: &str) -> Result<PrPolicyFile> {
    let raw = fs::read_to_string(path).map_err(|e| {
        Error::internal_io(
            format!("Failed to read PR policy file '{}': {}", path, e),
            Some(path.to_string()),
        )
    })?;

    let value: serde_json::Value = if path.ends_with(".json") {
        serde_json::from_str(&raw).map_err(|e| {
            Error::validation_invalid_argument(
                "policy",
                format!("PR policy JSON is invalid: {}", e),
                Some(path.to_string()),
                None,
            )
        })?
    } else {
        serde_yml::from_str(&raw).map_err(|e| {
            Error::validation_invalid_argument(
                "policy",
                format!("PR policy YAML is invalid: {}", e),
                Some(path.to_string()),
                None,
            )
        })?
    };

    let has_scoped_sections = value.get("open").is_some() || value.get("merge").is_some();
    if has_scoped_sections {
        serde_json::from_value(value).map_err(|e| {
            Error::validation_invalid_argument(
                "policy",
                format!("PR policy shape is invalid: {}", e),
                Some(path.to_string()),
                None,
            )
        })
    } else {
        // Back-compat for the original action policy file, which was a flat
        // merge gate. Apply it to both modes so existing files keep working.
        let mut rules: PrPolicyRules = serde_json::from_value(value).map_err(|e| {
            Error::validation_invalid_argument(
                "policy",
                format!("PR policy shape is invalid: {}", e),
                Some(path.to_string()),
                None,
            )
        })?;
        if rules.require_same_repository.is_none() {
            rules.require_same_repository = Some(true);
        }
        Ok(PrPolicyFile {
            open: PrPolicyRules::default(),
            merge: rules,
        })
    }
}

pub fn evaluate_open_policy(options: PrPolicyOpenOptions) -> Result<PrPolicyDecision> {
    let policy = load_policy(&options.policy_path)?;
    let files = if options.files_from_git {
        get_dirty_files(options.path.as_deref().unwrap_or("."))?
    } else {
        options.files
    };
    let context = PrPolicyContext {
        mode: PrPolicyMode::Open,
        source: non_empty(options.source),
        refs: PrPolicyTargetRefs {
            base: non_empty(options.refs.base),
            head: non_empty(options.refs.head),
            head_repository: non_empty(options.refs.head_repository),
            repository: non_empty(options.refs.repository),
        },
        files,
        path: options.path,
        ..Default::default()
    };
    Ok(evaluate_rules(&policy.open, context))
}

pub fn evaluate_merge_policy(options: PrPolicyMergeOptions) -> Result<PrPolicyDecision> {
    let policy = load_policy(&options.policy_path)?;
    let pr = pr_view(
        Some(&options.component_id),
        options.number,
        options.path.clone(),
    )?;
    let files = pr_files(
        Some(&options.component_id),
        options.number,
        options.path.clone(),
    )?;
    let merge_method = options.merge_method.unwrap_or_else(|| "squash".to_string());
    let mut decision = evaluate_rules(
        &policy.merge,
        PrPolicyContext {
            mode: PrPolicyMode::Merge,
            author: non_empty(options.author).or(pr.author),
            refs: PrPolicyTargetRefs {
                base: non_empty(options.refs.base).or(Some(pr.base)),
                head: non_empty(options.refs.head).or(Some(pr.head)),
                head_repository: non_empty(options.refs.head_repository).or(pr.head_repository),
                repository: non_empty(options.refs.repository)
                    .or(Some(format!("{}/{}", pr.owner, pr.repo))),
            },
            merge_method: Some(merge_method.clone()),
            files,
            path: options.path.clone(),
            ..Default::default()
        },
    );
    apply_ci_gate(
        &mut decision,
        &pr.ci_state,
        &pr.ci_summary,
        &pr.ci_next_action,
    );

    if options.merge && decision.allowed {
        pr_merge(
            Some(&options.component_id),
            PrMergeOptions {
                number: options.number,
                method: merge_method,
                delete_branch: policy.merge.delete_branch_on_merge.unwrap_or(true),
                path: options.path,
            },
        )?;
        decision.merged = Some(true);
        decision.report = format!("{} Merged.", decision.report);
    } else if options.merge {
        decision.merged = Some(false);
    }

    Ok(decision)
}

fn apply_ci_gate(
    decision: &mut PrPolicyDecision,
    ci_state: &str,
    ci_summary: &str,
    ci_next_action: &str,
) {
    decision.ci_state = Some(ci_state.to_string());
    decision.ci_summary = Some(ci_summary.to_string());
    decision.ci_next_action = Some(ci_next_action.to_string());
    decision.report = format!(
        "{}\n\nCI status: `{}` - {}.",
        decision.report, ci_state, ci_summary
    );

    let failure = match ci_state {
        "terminal_green" | "no_checks" => None,
        "terminal_failed" => Some("CI checks are terminal-failed"),
        "pending" => Some("CI checks are still pending"),
        "stale" => Some("CI check rollup is stale or eventually consistent after merge"),
        _ => Some("CI check state is unknown"),
    };

    if let Some(failure) = failure {
        decision.allowed = false;
        decision.safe = false;
        decision.reason = if decision.reason == "safe" {
            failure.to_string()
        } else {
            format!("{}; {}", decision.reason, failure)
        };
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn evaluate_rules(rules: &PrPolicyRules, context: PrPolicyContext) -> PrPolicyDecision {
    let mut failures = Vec::new();

    require_allowed(
        "source",
        context.source.as_deref(),
        &rules.allowed_sources,
        &mut failures,
    );
    require_allowed(
        "author",
        context.author.as_deref(),
        &rules.allowed_authors,
        &mut failures,
    );
    require_allowed(
        "base branch",
        context.refs.base.as_deref(),
        &rules.allowed_base_branches,
        &mut failures,
    );
    require_allowed(
        "head branch",
        context.refs.head.as_deref(),
        &rules.allowed_head_branches,
        &mut failures,
    );
    require_allowed(
        "head repository",
        context.refs.head_repository.as_deref(),
        &rules.allowed_head_repositories,
        &mut failures,
    );
    require_allowed(
        "merge method",
        context.merge_method.as_deref(),
        &rules.allowed_merge_methods,
        &mut failures,
    );

    if rules.require_same_repository.unwrap_or(false) {
        match (&context.refs.repository, &context.refs.head_repository) {
            (Some(repo), Some(head_repo)) if repo == head_repo => {}
            (Some(repo), Some(head_repo)) => failures.push(format!(
                "head repository {} does not match {}",
                head_repo, repo
            )),
            _ => failures.push("repository and head repository are required".to_string()),
        }
    }

    if let Some(max) = rules.max_changed_files {
        if context.files.len() > max {
            failures.push(format!(
                "{} changed files exceeds max_changed_files {}",
                context.files.len(),
                max
            ));
        }
    }

    let blocked_files: Vec<String> = context
        .files
        .iter()
        .filter(|file| matches_any(file, &rules.blocked_paths))
        .cloned()
        .collect();
    if !blocked_files.is_empty() {
        failures.push(format!(
            "blocked paths changed: {}",
            blocked_files.join(", ")
        ));
    }

    if !rules.allowed_paths.is_empty() {
        let unallowed: Vec<String> = context
            .files
            .iter()
            .filter(|file| !matches_any(file, &rules.allowed_paths))
            .cloned()
            .collect();
        if !unallowed.is_empty() {
            failures.push(format!(
                "paths outside allowed set changed: {}",
                unallowed.join(", ")
            ));
        }
    }

    if !rules.blocked_content_patterns.is_empty() {
        let root = context.path.as_deref().unwrap_or(".");
        let content_hits =
            files_with_blocked_content(root, &context.files, &rules.blocked_content_patterns);
        if !content_hits.is_empty() {
            failures.push(format!(
                "blocked content patterns found in: {}",
                content_hits.join(", ")
            ));
        }
    }

    let mode = match context.mode {
        PrPolicyMode::Open => "open",
        PrPolicyMode::Merge => "merge",
    };
    let allowed = failures.is_empty();
    let reason = if allowed {
        "safe".to_string()
    } else {
        failures.join("; ")
    };
    let title = rules.title.as_deref().unwrap_or("PR policy");
    let report = if allowed {
        format!(
            "## {}\n\nSafe for {}: {} changed file(s) matched policy.",
            title,
            mode,
            context.files.len()
        )
    } else {
        format!("## {}\n\nUnsafe for {}: {}", title, mode, reason)
    };

    PrPolicyDecision {
        mode: mode.to_string(),
        allowed,
        safe: allowed,
        reason,
        report,
        changed_file_count: context.files.len(),
        files: context.files,
        ci_state: None,
        ci_summary: None,
        ci_next_action: None,
        merged: None,
    }
}

fn require_allowed(
    label: &str,
    value: Option<&str>,
    patterns: &[String],
    failures: &mut Vec<String>,
) {
    if patterns.is_empty() {
        return;
    }
    match value {
        Some(value) if matches_any(value, patterns) => {}
        Some(value) => failures.push(format!("{} {} is not allowed", label, value)),
        None => failures.push(format!("{} is required", label)),
    }
}

fn matches_any(value: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| value == pattern || glob_match::glob_match(pattern, value))
}

fn files_with_blocked_content(root: &str, files: &[String], patterns: &[String]) -> Vec<String> {
    let regexes: Vec<regex::Regex> = patterns
        .iter()
        .filter_map(|pattern| regex::Regex::new(pattern).ok())
        .collect();
    files
        .iter()
        .filter(|file| {
            let path = std::path::Path::new(root).join(file);
            let Ok(content) = fs::read_to_string(path) else {
                return false;
            };
            regexes.iter().any(|re| re.is_match(&content))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluate_rules() {
        let rules = PrPolicyRules {
            allowed_sources: vec!["autofix".into()],
            allowed_head_branches: vec!["chore/homeboy-*".into()],
            allowed_paths: vec!["src/**".into(), "tests/**".into()],
            max_changed_files: Some(2),
            ..Default::default()
        };
        let decision = evaluate_rules(
            &rules,
            PrPolicyContext {
                mode: PrPolicyMode::Open,
                source: Some("autofix".into()),
                refs: PrPolicyTargetRefs {
                    head: Some("chore/homeboy-autofix".into()),
                    ..Default::default()
                },
                files: vec!["src/lib.rs".into(), "tests/lib.rs".into()],
                ..Default::default()
            },
        );
        assert!(decision.allowed);
    }

    #[test]
    fn merge_policy_blocks_unallowed_files() {
        let rules = PrPolicyRules {
            allowed_paths: vec!["src/**".into()],
            blocked_paths: vec!["src/unsafe/**".into()],
            ..Default::default()
        };
        let decision = evaluate_rules(
            &rules,
            PrPolicyContext {
                mode: PrPolicyMode::Merge,
                files: vec![
                    "src/lib.rs".into(),
                    "src/unsafe/key.rs".into(),
                    "README.md".into(),
                ],
                ..Default::default()
            },
        );
        assert!(!decision.allowed);
        assert!(decision.reason.contains("blocked paths changed"));
        assert!(decision.reason.contains("paths outside allowed set"));
    }

    #[test]
    fn require_same_repository_blocks_forks() {
        let rules = PrPolicyRules {
            require_same_repository: Some(true),
            ..Default::default()
        };
        let decision = evaluate_rules(
            &rules,
            PrPolicyContext {
                refs: PrPolicyTargetRefs {
                    repository: Some("Extra-Chill/homeboy".into()),
                    head_repository: Some("someone/homeboy".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(!decision.allowed);
        assert!(decision.reason.contains("does not match"));
    }

    #[test]
    fn test_load_policy_reads_scoped_yaml() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("policy.yml");
        fs::write(
            &path,
            "open:\n  allowed_sources: [autofix]\nmerge:\n  allowed_authors: ['homeboy-ci[bot]']\n",
        )
        .expect("write policy");

        let policy = load_policy(path.to_str().expect("utf8 path")).expect("policy parses");
        assert_eq!(policy.open.allowed_sources, vec!["autofix"]);
        assert_eq!(policy.merge.allowed_authors, vec!["homeboy-ci[bot]"]);
    }

    #[test]
    fn test_evaluate_open_policy_reads_explicit_files() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("policy.json");
        fs::write(
            &path,
            r#"{"open":{"allowed_sources":["autofix"],"allowed_paths":["src/**"]}}"#,
        )
        .expect("write policy");

        let decision = evaluate_open_policy(PrPolicyOpenOptions {
            component_id: "homeboy".into(),
            policy_path: path.to_string_lossy().to_string(),
            source: Some("autofix".into()),
            files: vec!["src/lib.rs".into()],
            ..Default::default()
        })
        .expect("policy evaluates");

        assert!(decision.allowed);
    }

    #[test]
    fn test_evaluate_merge_policy_requires_github_metadata() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("policy.json");
        fs::write(&path, r#"{"merge":{}}"#).expect("write policy");

        let result = evaluate_merge_policy(PrPolicyMergeOptions {
            component_id: "definitely-missing-component".into(),
            policy_path: path.to_string_lossy().to_string(),
            number: 1,
            ..Default::default()
        });

        assert!(result.is_err());
    }

    #[test]
    fn ci_gate_allows_terminal_green() {
        let mut decision = PrPolicyDecision {
            mode: "merge".into(),
            allowed: true,
            safe: true,
            reason: "safe".into(),
            report: "## Policy\n\nSafe".into(),
            changed_file_count: 0,
            files: Vec::new(),
            ci_state: None,
            ci_summary: None,
            ci_next_action: None,
            merged: None,
        };

        apply_ci_gate(
            &mut decision,
            "terminal_green",
            "1 reported check(s): 1 passed, 0 failed/unknown, 0 queued, 0 running, 0 pending, 0 skipped",
            "merge_ready",
        );

        assert!(decision.allowed);
        assert_eq!(decision.ci_state.as_deref(), Some("terminal_green"));
        assert_eq!(decision.ci_next_action.as_deref(), Some("merge_ready"));
        assert!(decision.report.contains("CI status: `terminal_green`"));
    }

    #[test]
    fn ci_gate_blocks_stale_or_pending_checks_before_merge() {
        let mut decision = PrPolicyDecision {
            mode: "merge".into(),
            allowed: true,
            safe: true,
            reason: "safe".into(),
            report: "## Policy\n\nSafe".into(),
            changed_file_count: 0,
            files: Vec::new(),
            ci_state: None,
            ci_summary: None,
            ci_next_action: None,
            merged: None,
        };

        apply_ci_gate(
            &mut decision,
            "stale",
            "1 reported check(s): 0 passed, 0 failed/unknown, 0 queued, 0 running, 1 pending, 0 skipped",
            "wait",
        );

        assert!(!decision.allowed);
        assert!(!decision.safe);
        assert!(decision.reason.contains("stale"));
        assert_eq!(decision.ci_state.as_deref(), Some("stale"));
    }
}
