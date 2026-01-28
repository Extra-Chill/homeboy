//! Documentation audit system for extracting and verifying claims from markdown files.
//!
//! This module provides a claim-based approach to documentation auditing:
//! 1. Extract claims from documentation (file paths, identifiers, code examples)
//! 2. Verify claims against the actual codebase
//! 3. Build actionable task lists for agents to execute

mod claims;
mod tasks;
mod verify;

use std::fs;
use std::path::Path;

pub use claims::{Claim, ClaimType};
pub use tasks::{AuditTask, AuditTaskStatus};
pub use verify::VerifyResult;

use crate::{component, git, module, Result};

/// Summary of an audit operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditSummary {
    pub docs_scanned: usize,
    pub claims_extracted: usize,
    pub verified: usize,
    pub broken: usize,
    pub needs_verification: usize,
}

/// Context about recent changes that may affect documentation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangesContext {
    pub commits_since_tag: usize,
    pub changed_files: Vec<String>,
    pub priority_docs: Vec<String>,
}

/// Result of auditing a component's documentation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditResult {
    pub component_id: String,
    pub summary: AuditSummary,
    pub tasks: Vec<AuditTask>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes_context: Option<ChangesContext>,
}

/// Audit a component's documentation and return structured tasks.
pub fn audit_component(component_id: &str) -> Result<AuditResult> {
    let comp = component::load(component_id)?;
    let source_path = Path::new(&comp.local_path);
    let docs_path = source_path.join("docs");

    // Get changelog target to exclude from audit (historical references are expected)
    let changelog_exclude = comp.changelog_target.as_deref();

    // Collect ignore patterns from all linked modules
    let ignore_patterns = collect_module_ignore_patterns(&comp);

    // Find all documentation files (excluding changelog)
    let doc_files = find_doc_files(&docs_path, changelog_exclude);
    let docs_scanned = doc_files.len();

    // Extract claims from all docs
    let mut all_claims = Vec::new();
    for doc_file in &doc_files {
        let doc_path = docs_path.join(doc_file);
        if let Ok(content) = fs::read_to_string(&doc_path) {
            let claims = claims::extract_claims(&content, doc_file, &ignore_patterns);
            all_claims.extend(claims);
        }
    }

    let claims_extracted = all_claims.len();

    // Verify claims and build tasks
    let mut tasks = Vec::new();
    let mut verified = 0usize;
    let mut broken = 0usize;
    let mut needs_verification = 0usize;

    for claim in all_claims {
        let result = verify::verify_claim(&claim, source_path, &docs_path, Some(component_id));
        let task = tasks::build_task(claim, result);

        match task.status {
            AuditTaskStatus::Verified => verified += 1,
            AuditTaskStatus::Broken => broken += 1,
            AuditTaskStatus::NeedsVerification => needs_verification += 1,
        }

        tasks.push(task);
    }

    // Get changes context if available
    let changes_context = build_changes_context(component_id, &tasks);

    Ok(AuditResult {
        component_id: component_id.to_string(),
        summary: AuditSummary {
            docs_scanned,
            claims_extracted,
            verified,
            broken,
            needs_verification,
        },
        tasks,
        changes_context,
    })
}

/// Find all markdown files in the docs directory.
///
/// Excludes the changelog file if configured, since changelogs contain
/// historical references to file paths that may no longer exist.
fn find_doc_files(docs_path: &Path, exclude_changelog: Option<&str>) -> Vec<String> {
    let mut docs = Vec::new();

    if !docs_path.exists() {
        return docs;
    }

    // Extract changelog filename for comparison
    let changelog_filename = exclude_changelog
        .and_then(|p| Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_lowercase());

    fn scan_docs(
        dir: &Path,
        prefix: &str,
        docs: &mut Vec<String>,
        changelog_filename: &Option<String>,
    ) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();

                if name.starts_with('.') {
                    continue;
                }

                if path.is_file() && name.ends_with(".md") {
                    // Skip changelog file if configured
                    if let Some(changelog) = changelog_filename {
                        if name.to_lowercase() == *changelog {
                            continue;
                        }
                    }

                    let relative = if prefix.is_empty() {
                        name
                    } else {
                        format!("{}/{}", prefix, name)
                    };
                    docs.push(relative);
                } else if path.is_dir() {
                    let new_prefix = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", prefix, name)
                    };
                    scan_docs(&path, &new_prefix, docs, changelog_filename);
                }
            }
        }
    }

    scan_docs(docs_path, "", &mut docs, &changelog_filename);
    docs.sort();
    docs
}

/// Build changes context by checking recent git changes.
fn build_changes_context(component_id: &str, tasks: &[AuditTask]) -> Option<ChangesContext> {
    // Try to get changes - ignore errors (component may not have git history)
    let changes = match git::changes(Some(component_id), None, false) {
        Ok(c) => c,
        Err(_) => return None,
    };

    if changes.commits.is_empty() && !changes.uncommitted.has_changes {
        return None;
    }

    // Collect all changed files from uncommitted changes
    let mut changed_files: Vec<String> = Vec::new();
    changed_files.extend(changes.uncommitted.staged.iter().cloned());
    changed_files.extend(changes.uncommitted.unstaged.iter().cloned());
    changed_files.extend(changes.uncommitted.untracked.iter().cloned());
    changed_files.sort();
    changed_files.dedup();

    // Find docs that reference changed files
    let mut priority_docs: Vec<String> = tasks
        .iter()
        .filter(|task| {
            // Check if any changed file matches the claim
            if let ClaimType::FilePath = task.claim_type {
                changed_files
                    .iter()
                    .any(|f: &String| task.claim_value.contains(f) || f.contains(&task.claim_value))
            } else {
                false
            }
        })
        .map(|task| task.doc.clone())
        .collect();

    priority_docs.sort();
    priority_docs.dedup();

    Some(ChangesContext {
        commits_since_tag: changes.commits.len(),
        changed_files,
        priority_docs,
    })
}

/// Collect audit ignore patterns from all linked modules.
fn collect_module_ignore_patterns(comp: &component::Component) -> Vec<String> {
    let mut patterns = Vec::new();
    if let Some(ref modules) = comp.modules {
        for module_id in modules.keys() {
            if let Ok(manifest) = module::load_module(module_id) {
                patterns.extend(manifest.audit_ignore_claim_patterns.clone());
            }
        }
    }
    patterns
}
