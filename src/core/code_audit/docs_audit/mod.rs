//! Documentation audit system for extracting and verifying claims from markdown files.
//!
//! This extension provides a doc-centric approach to documentation auditing:
//! 1. Extract claims from documentation (file paths, identifiers, code examples)
//! 2. Verify claims against the actual codebase
//! 3. Correlate docs with git changes to identify priority docs needing review
//! 4. Build an alignment report focused on actionable items

pub mod baseline;
pub(crate) mod claims;
mod tasks;
pub(crate) mod verify;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

pub use claims::{Claim, ClaimConfidence, ClaimType};
pub use tasks::{AuditTask, AuditTaskStatus};
pub use verify::VerifyResult;

use regex::Regex;

use crate::{component, extension, git, is_zero, scope, Result};

/// A doc that needs content review due to referenced files changing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PriorityDoc {
    pub doc: String,
    pub reason: String,
    pub changed_files_referenced: Vec<String>,
    pub code_examples: usize,
    pub action: String,
}

/// A feature found in source code with no mention in documentation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UndocumentedFeature {
    pub name: String,
    pub source_file: String,
    pub line: usize,
    pub pattern: String,
}

/// A feature detected in source code (documented or not).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DetectedFeature {
    pub name: String,
    pub source_file: String,
    pub line: usize,
    pub pattern: String,
    pub documented: bool,
    /// Doc comment lines found directly above the feature (e.g. `///`, `/**`, `#`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Fields or items inside the feature's block (struct fields, enum variants, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FeatureField>>,
}

/// A field or item inside a detected feature's block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeatureField {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A broken reference that needs fixing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrokenReference {
    pub doc: String,
    pub line: usize,
    pub claim: String,
    pub confidence: ClaimConfidence,
    /// Surrounding lines from the doc file for context (up to 3 lines around the reference).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_context: Option<Vec<String>>,
    pub action: String,
}

/// Summary counts for the alignment report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlignmentSummary {
    pub docs_scanned: usize,
    pub priority_docs: usize,
    pub broken_references: usize,
    pub unchanged_docs: usize,
    /// Total features detected by extension-defined patterns (omitted when 0).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_features: usize,
    /// Features with at least one mention in documentation (omitted when 0).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub documented_features: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub undocumented_features: usize,
}

/// Result of auditing a component's documentation for content alignment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditResult {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    pub summary: AlignmentSummary,
    pub changed_files: Vec<String>,
    pub priority_docs: Vec<PriorityDoc>,
    pub broken_references: Vec<BrokenReference>,
    pub undocumented_features: Vec<UndocumentedFeature>,
    /// All detected features (only populated when `--features` flag is set).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub detected_features: Vec<DetectedFeature>,
}

/// Audit documentation at a direct filesystem path without a registered component.
///
/// Uses the directory name as the label and defaults to "docs" for the docs
/// directory. Extension patterns and changelog exclusion are not available.
/// When `include_features` is true, the full detected features list is included.
pub fn audit_path(
    path: &str,
    docs_dir_override: Option<&str>,
    include_features: bool,
) -> Result<AuditResult> {
    let source_path = Path::new(path);
    if !source_path.is_dir() {
        return Err(crate::Error::validation_invalid_argument(
            "path",
            format!("'{}' is not a directory", path),
            Some(path.to_string()),
            None,
        ));
    }

    let label = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let docs_dir = docs_dir_override.unwrap_or("docs");
    let docs_dirs = vec![docs_dir.to_string()];
    let docs_path = source_path.join(docs_dir);

    let doc_files = find_doc_files(&docs_path, &[]);
    let docs_scanned = doc_files.len();

    let mut all_claims = Vec::new();
    let mut doc_contents: HashMap<String, String> = HashMap::new();
    for doc_file in &doc_files {
        let doc_path = docs_path.join(doc_file);
        if let Ok(content) = fs::read_to_string(&doc_path) {
            let claims = claims::extract_claims(&content, doc_file, &[]);
            all_claims.extend(claims);
            doc_contents.insert(doc_file.clone(), content);
        }
    }

    let mut tasks = Vec::new();
    for claim in all_claims {
        let result = verify::verify_claim(&claim, source_path, &docs_path, None);
        let task = tasks::build_task(claim, result);
        tasks.push(task);
    }

    // Get uncommitted changes directly from the path's git repo
    let changed_files = git::get_uncommitted_changes(path)
        .map(|u| {
            let mut files = Vec::new();
            files.extend(u.staged);
            files.extend(u.unstaged);
            files.sort();
            files.dedup();
            files
        })
        .unwrap_or_default();

    let priority_docs = build_priority_docs(&tasks, &changed_files);
    let broken_references = extract_broken_references(&tasks, &doc_contents);

    let feature_result = detect_features(&[], source_path, &docs_dirs, &[], &HashMap::new());

    let docs_with_issues: HashSet<_> = priority_docs
        .iter()
        .map(|p| &p.doc)
        .chain(broken_references.iter().map(|b| &b.doc))
        .collect();
    let unchanged_docs = docs_scanned.saturating_sub(docs_with_issues.len());

    Ok(AuditResult {
        component_id: label,
        baseline_ref: None,
        summary: AlignmentSummary {
            docs_scanned,
            priority_docs: priority_docs.len(),
            broken_references: broken_references.len(),
            unchanged_docs,
            total_features: feature_result.total,
            documented_features: feature_result.documented,
            undocumented_features: feature_result.undocumented.len(),
        },
        changed_files,
        priority_docs,
        broken_references,
        undocumented_features: feature_result.undocumented,
        detected_features: if include_features {
            feature_result.all_features
        } else {
            Vec::new()
        },
    })
}

/// Audit a component's documentation and return an alignment report.
///
/// If `docs_dir_override` is provided, it's used instead of the component's
/// configured `docs_dir`/`docs_dirs` (which defaults to "docs").
/// When `include_features` is true, the full detected features list is included.
pub fn audit_component(
    component_id: &str,
    docs_dir_override: Option<&str>,
    include_features: bool,
) -> Result<AuditResult> {
    let comp = component::load(component_id)?;
    let source_path = Path::new(&comp.local_path);

    // Resolve docs directories: CLI override > docs_dirs > docs_dir > default "docs"
    let docs_dirs: Vec<String> = if let Some(override_dir) = docs_dir_override {
        vec![override_dir.to_string()]
    } else if !comp.docs_dirs.is_empty() {
        comp.docs_dirs.clone()
    } else {
        vec![comp.docs_dir.as_deref().unwrap_or("docs").to_string()]
    };

    // Primary docs path (first dir) for claim verification and priority docs
    let docs_path = source_path.join(&docs_dirs[0]);

    let effective_scope = scope::resolve_component_scope(&comp, scope::ScopeCommand::Audit);

    // Collect ignore patterns from all linked extensions
    let ignore_patterns = collect_extension_ignore_patterns(&comp);

    // Collect feature patterns from all linked extensions
    let feature_patterns = collect_extension_feature_patterns(&comp);

    // Find all documentation files (excluding changelog)
    let doc_files = find_doc_files(&docs_path, &effective_scope.exclude);
    let docs_scanned = doc_files.len();

    // Extract claims from all docs (keep content for context extraction)
    let mut all_claims = Vec::new();
    let mut doc_contents: HashMap<String, String> = HashMap::new();
    for doc_file in &doc_files {
        let doc_path = docs_path.join(doc_file);
        if let Ok(content) = fs::read_to_string(&doc_path) {
            let claims = claims::extract_claims(&content, doc_file, &ignore_patterns);
            all_claims.extend(claims);
            doc_contents.insert(doc_file.clone(), content);
        }
    }

    // Verify claims and build tasks (internal only)
    let mut tasks = Vec::new();
    for claim in all_claims {
        let result = verify::verify_claim(&claim, source_path, &docs_path, Some(component_id));
        let task = tasks::build_task(claim, result);
        tasks.push(task);
    }

    // Get changed files from git (both committed and uncommitted)
    let (changed_files, baseline_ref) = get_changed_files(component_id);

    // Build doc-centric outputs
    let priority_docs = build_priority_docs(&tasks, &changed_files);
    let broken_references = extract_broken_references(&tasks, &doc_contents);

    // Collect feature context extraction rules from extensions
    let context_rules = collect_extension_feature_context(&comp);

    // Detect features and documentation coverage across all source files
    let feature_result = detect_features(
        &feature_patterns,
        source_path,
        &docs_dirs,
        &effective_scope.exclude,
        &context_rules,
    );

    // Calculate unchanged docs (docs with no priority items and no broken refs)
    let docs_with_issues: HashSet<_> = priority_docs
        .iter()
        .map(|p| &p.doc)
        .chain(broken_references.iter().map(|b| &b.doc))
        .collect();
    let unchanged_docs = docs_scanned.saturating_sub(docs_with_issues.len());

    Ok(AuditResult {
        component_id: component_id.to_string(),
        baseline_ref,
        summary: AlignmentSummary {
            docs_scanned,
            priority_docs: priority_docs.len(),
            broken_references: broken_references.len(),
            unchanged_docs,
            total_features: feature_result.total,
            documented_features: feature_result.documented,
            undocumented_features: feature_result.undocumented.len(),
        },
        changed_files,
        priority_docs,
        broken_references,
        undocumented_features: feature_result.undocumented,
        detected_features: if include_features {
            feature_result.all_features
        } else {
            Vec::new()
        },
    })
}

/// Find all markdown files in the docs directory.
///
/// Excludes configured doc targets using file-name matching (case-insensitive).
pub(crate) fn find_doc_files(docs_path: &Path, excluded_targets: &[String]) -> Vec<String> {
    let mut docs = Vec::new();

    if !docs_path.exists() {
        return docs;
    }

    let excluded_filenames: std::collections::HashSet<String> = excluded_targets
        .iter()
        .filter_map(|p| Path::new(p).file_name())
        .filter_map(|n| n.to_str())
        .map(|s| s.to_lowercase())
        .collect();

    fn scan_docs(
        dir: &Path,
        prefix: &str,
        docs: &mut Vec<String>,
        excluded_filenames: &std::collections::HashSet<String>,
    ) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();

                if name.starts_with('.') {
                    continue;
                }

                if path.is_file() && name.ends_with(".md") {
                    if excluded_filenames.contains(&name.to_lowercase()) {
                        continue;
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
                    scan_docs(&path, &new_prefix, docs, excluded_filenames);
                }
            }
        }
    }

    scan_docs(docs_path, "", &mut docs, &excluded_filenames);
    docs.sort();
    docs
}

/// Get changed files from git, including both uncommitted and committed changes.
/// Returns (changed_files, baseline_ref).
fn get_changed_files(component_id: &str) -> (Vec<String>, Option<String>) {
    // Request diff output to extract committed file changes
    let changes = match git::changes(Some(component_id), None, true) {
        Ok(c) => c,
        Err(_) => return (vec![], None),
    };

    let mut files: Vec<String> = Vec::new();

    // Uncommitted changes
    files.extend(changes.uncommitted.staged.iter().cloned());
    files.extend(changes.uncommitted.unstaged.iter().cloned());

    // Parse committed changes from diff output
    if let Some(ref diff) = changes.diff {
        files.extend(parse_diff_file_paths(diff));
    }

    files.sort();
    files.dedup();

    (files, changes.baseline_ref)
}

/// Parse git diff output to extract changed file paths.
fn parse_diff_file_paths(diff: &str) -> Vec<String> {
    diff.lines()
        .filter(|line| line.starts_with("diff --git "))
        .filter_map(|line| {
            // Format: "diff --git a/path/to/file b/path/to/file"
            line.split(" b/").nth(1).map(|s| s.to_string())
        })
        .collect()
}

/// Build priority docs by grouping tasks by doc and filtering for changed file references.
fn build_priority_docs(tasks: &[AuditTask], changed_files: &[String]) -> Vec<PriorityDoc> {
    // Group tasks by doc file
    let mut docs_map: HashMap<String, Vec<&AuditTask>> = HashMap::new();
    for task in tasks {
        docs_map.entry(task.doc.clone()).or_default().push(task);
    }

    let mut priority_docs: Vec<PriorityDoc> = Vec::new();

    for (doc, doc_tasks) in docs_map {
        // Find which changed files this doc references
        let referenced_changes: Vec<String> = changed_files
            .iter()
            .filter(|f| doc_tasks.iter().any(|t| references_file(&t.claim_value, f)))
            .cloned()
            .collect();

        if referenced_changes.is_empty() {
            continue; // Not a priority doc
        }

        // Count code examples in this doc
        let code_examples = doc_tasks
            .iter()
            .filter(|t| matches!(t.claim_type, ClaimType::CodeExample))
            .count();

        // Build action based on what needs review
        let action = build_doc_action(&referenced_changes, code_examples);

        priority_docs.push(PriorityDoc {
            doc,
            reason: format!(
                "{} referenced source file(s) changed since baseline",
                referenced_changes.len()
            ),
            changed_files_referenced: referenced_changes,
            code_examples,
            action,
        });
    }

    // Sort by impact (most changed files referenced first)
    priority_docs.sort_by(|a, b| {
        b.changed_files_referenced
            .len()
            .cmp(&a.changed_files_referenced.len())
    });

    priority_docs
}

/// Check if a claim value references a changed file.
fn references_file(claim_value: &str, changed_file: &str) -> bool {
    let claim_normalized = claim_value.trim_start_matches('/');
    let file_normalized = changed_file.trim_start_matches('/');

    // Exact path match
    if claim_normalized == file_normalized {
        return true;
    }

    // Directory contains changed file (claim is a directory path like "inc/Engine/")
    if claim_value.ends_with('/') && file_normalized.starts_with(claim_normalized) {
        return true;
    }

    // Basename match (for code examples that reference "ToolExecutor" without full path)
    if let Some(basename) = Path::new(changed_file).file_stem() {
        if let Some(name) = basename.to_str() {
            // Only match if the claim contains the basename as a significant reference
            // Avoid false positives by requiring the name to appear as a word boundary
            if claim_value.contains(name) && name.len() >= 4 {
                return true;
            }
        }
    }

    false
}

/// Build an action description for a priority doc.
fn build_doc_action(changed_files: &[String], code_examples: usize) -> String {
    let files_desc = if changed_files.len() == 1 {
        changed_files[0].clone()
    } else {
        format!("{} files", changed_files.len())
    };

    if code_examples > 0 {
        format!(
            "Documentation may be stale: {} code example(s) reference changed source ({}). Update docs to match current implementation.",
            code_examples, files_desc
        )
    } else {
        format!(
            "Source changed ({}). Review documentation for accuracy against current implementation.",
            files_desc
        )
    }
}

/// Extract broken references from tasks into a separate list.
///
/// Includes surrounding lines from the doc file for context when available.
fn extract_broken_references(
    tasks: &[AuditTask],
    doc_contents: &HashMap<String, String>,
) -> Vec<BrokenReference> {
    tasks
        .iter()
        .filter(|t| matches!(t.status, AuditTaskStatus::Broken))
        .map(|t| {
            let doc_context = extract_doc_context(doc_contents, &t.doc, t.line);
            BrokenReference {
                doc: t.doc.clone(),
                line: t.line,
                claim: t.claim.clone(),
                confidence: t.confidence.clone(),
                doc_context,
                action: t.action.clone().unwrap_or_else(|| {
                    "Stale reference. Update or remove from documentation.".to_string()
                }),
            }
        })
        .collect()
}

/// Extract surrounding lines from a doc file for context.
///
/// Returns up to 3 lines centered on the target line (1 before, target, 1 after).
/// Each line is prefixed with its line number for easy navigation.
fn extract_doc_context(
    doc_contents: &HashMap<String, String>,
    doc_file: &str,
    line: usize,
) -> Option<Vec<String>> {
    let content = doc_contents.get(doc_file)?;
    let lines: Vec<&str> = content.lines().collect();

    if line == 0 || line > lines.len() {
        return None;
    }

    let line_idx = line - 1; // 0-indexed
    let start = line_idx.saturating_sub(1);
    let end = (line_idx + 2).min(lines.len()); // exclusive, up to 1 line after

    let context: Vec<String> = (start..end)
        .map(|i| format!("{}: {}", i + 1, lines[i]))
        .collect();

    if context.is_empty() {
        None
    } else {
        Some(context)
    }
}

/// Collect feature detection patterns from all linked extensions.
fn collect_extension_feature_patterns(comp: &component::Component) -> Vec<String> {
    let mut patterns = Vec::new();
    if let Some(ref extensions) = comp.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
                patterns.extend(manifest.audit_feature_patterns().to_vec());
            }
        }
    }
    patterns
}

/// Collect feature context extraction rules from all linked extensions.
fn collect_extension_feature_context(
    comp: &component::Component,
) -> HashMap<String, extension::FeatureContextRule> {
    let mut rules = HashMap::new();
    if let Some(ref extensions) = comp.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
                for (key, rule) in manifest.audit_feature_context() {
                    rules.insert(key.clone(), rule.clone());
                }
            }
        }
    }
    rules
}

/// Result of feature detection including coverage counts.
struct FeatureDetectionResult {
    /// Total unique feature names found in source code.
    total: usize,
    /// Features that have at least one mention in documentation.
    documented: usize,
    /// Features with no documentation mention.
    undocumented: Vec<UndocumentedFeature>,
    /// All detected features (documented and undocumented).
    all_features: Vec<DetectedFeature>,
}

/// Extract doc comment lines above a byte position in source content.
///
/// Walks backwards from the match position, collecting lines that start with
/// doc comment markers: `///`, `//!`, `*`, `/**`, `#` (PHP/Python), `--` (SQL/Lua).
/// Strips the comment markers and returns the combined text.
fn extract_doc_comment(content: &str, byte_pos: usize) -> Option<String> {
    // Find the line containing byte_pos
    let before = &content[..byte_pos];
    let lines: Vec<&str> = before.lines().collect();

    if lines.is_empty() {
        return None;
    }

    let mut comment_lines: Vec<String> = Vec::new();

    // Walk backwards from the line before the match
    for line in lines.iter().rev() {
        let trimmed = line.trim();

        // Rust/JS/TS doc comments
        if let Some(text) = trimmed.strip_prefix("///") {
            comment_lines.push(text.trim().to_string());
        } else if let Some(text) = trimmed.strip_prefix("//!") {
            comment_lines.push(text.trim().to_string());
        }
        // Block comment continuation
        else if let Some(text) = trimmed.strip_prefix("* ") {
            // Skip opening /** and closing */
            if !trimmed.starts_with("/**") && !trimmed.starts_with("*/") {
                comment_lines.push(text.trim().to_string());
            }
        } else if trimmed == "*" {
            // Empty line in block comment
            comment_lines.push(String::new());
        }
        // PHP/Python doc comment openers
        else if trimmed.starts_with("/**") || trimmed.starts_with("\"\"\"") {
            // Opening line of a block — may have text after marker
            if let Some(text) = trimmed.strip_prefix("/**") {
                let text = text.trim();
                if !text.is_empty() && text != "*" {
                    comment_lines.push(text.to_string());
                }
            }
            break; // We've found the start of the block
        }
        // Python/Ruby comments
        else if let Some(text) = trimmed.strip_prefix("# ") {
            comment_lines.push(text.trim().to_string());
        } else if trimmed == "#" {
            comment_lines.push(String::new());
        }
        // Attribute lines (skip, keep going)
        else if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
            continue;
        }
        // Empty lines between attributes and doc comments — keep going
        else if trimmed.is_empty() {
            // If we already have comment lines, an empty line means we've left the comment block
            if !comment_lines.is_empty() {
                break;
            }
            continue;
        }
        // Anything else means we've left the comment block
        else {
            break;
        }
    }

    if comment_lines.is_empty() {
        return None;
    }

    // Reverse since we walked backwards
    comment_lines.reverse();

    // Join and clean up
    let text = comment_lines.join(" ").trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Extract field names and their doc comments from a block following a byte position.
///
/// Finds the opening `{` after the match, then extracts each field/variant until
/// the matching closing `}`. Handles Rust struct fields, enum variants, and
/// generic key-value patterns.
fn extract_block_fields(content: &str, byte_pos: usize) -> Option<Vec<FeatureField>> {
    let after = &content[byte_pos..];

    // Find the opening brace
    let brace_offset = after.find('{')?;
    let block_start = byte_pos + brace_offset + 1;

    // Find the matching closing brace (tracking nesting)
    let mut depth = 1;
    let mut pos = block_start;
    let bytes = content.as_bytes();
    while pos < content.len() && depth > 0 {
        match bytes[pos] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        if depth > 0 {
            pos += 1;
        }
    }

    if depth != 0 {
        return None; // Unbalanced braces
    }

    let block_content = &content[block_start..pos];
    let lines: Vec<&str> = block_content.lines().collect();

    let mut fields = Vec::new();
    let mut pending_doc: Vec<String> = Vec::new();

    for line in &lines {
        let trimmed = line.trim();

        // Collect doc comments for the next field
        if let Some(text) = trimmed.strip_prefix("///") {
            pending_doc.push(text.trim().to_string());
            continue;
        }

        // Skip attributes
        if trimmed.starts_with("#[") || trimmed.is_empty() {
            continue;
        }

        // Try to extract a field name
        // Strip visibility: pub, pub(crate), pub(super)
        let rest = trimmed
            .strip_prefix("pub(crate) ")
            .or_else(|| trimmed.strip_prefix("pub(super) "))
            .or_else(|| trimmed.strip_prefix("pub "))
            .unwrap_or(trimmed);

        // Extract identifier (up to :, (, =, <, ,, {, or whitespace)
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if name.is_empty() {
            pending_doc.clear();
            continue;
        }

        // Must look like a field declaration or enum variant
        let after_name = rest[name.len()..].trim_start();
        let is_field = after_name.starts_with(':')
            || after_name.starts_with('(')
            || after_name.starts_with('{')
            || after_name.starts_with(',')
            || after_name.is_empty()
            || after_name == ","
            || after_name.starts_with("//");

        if is_field {
            let description = if pending_doc.is_empty() {
                None
            } else {
                Some(pending_doc.join(" "))
            };

            fields.push(FeatureField { name, description });
        }

        pending_doc.clear();
    }

    if fields.is_empty() {
        None
    } else {
        Some(fields)
    }
}

/// Detect features across all source files and report documentation coverage.
///
/// Scans the entire source tree (excluding vendor/node_modules/test dirs) for
/// feature registrations matching the configured patterns. Returns counts of
/// total, documented, and undocumented features.
///
/// Documentation is gathered from:
/// 1. All configured docs directories
/// 2. README.md and README.txt in the project root (auto-included)
fn detect_features(
    feature_patterns: &[String],
    source_path: &Path,
    docs_dirs: &[String],
    excluded_targets: &[String],
    context_rules: &HashMap<String, extension::FeatureContextRule>,
) -> FeatureDetectionResult {
    let empty = FeatureDetectionResult {
        total: 0,
        documented: 0,
        undocumented: Vec::new(),
        all_features: Vec::new(),
    };

    if feature_patterns.is_empty() {
        return empty;
    }

    // Compile regexes once
    let compiled: Vec<(Regex, String)> = feature_patterns
        .iter()
        .filter_map(|p| Regex::new(p).ok().map(|r| (r, p.clone())))
        .collect();

    if compiled.is_empty() {
        return empty;
    }

    // Collect all doc content from all configured directories
    let mut all_doc_parts: Vec<String> = Vec::new();

    for docs_dir in docs_dirs {
        let docs_path = source_path.join(docs_dir);
        let doc_files = find_doc_files(&docs_path, excluded_targets);
        for f in &doc_files {
            if let Ok(content) = fs::read_to_string(docs_path.join(f)) {
                all_doc_parts.push(content);
            }
        }
    }

    // Auto-include README files from project root
    for readme in &["README.md", "readme.md", "README.txt", "readme.txt"] {
        let readme_path = source_path.join(readme);
        if readme_path.exists() {
            if let Ok(content) = fs::read_to_string(&readme_path) {
                all_doc_parts.push(content);
            }
        }
    }

    let all_doc_content = all_doc_parts.join("\n");

    // Find all source files in the project (excluding common non-source dirs)
    let source_files = find_source_files(source_path);

    let mut undocumented = Vec::new();
    let mut all_features = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut documented_count: usize = 0;

    for file in &source_files {
        let file_path = source_path.join(file);
        let content = match fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Build line offset table for mapping byte positions to line numbers
        let line_offsets: Vec<usize> = std::iter::once(0)
            .chain(content.match_indices('\n').map(|(i, _)| i + 1))
            .collect();

        for (regex, pattern) in &compiled {
            // Search the full file content (not line-by-line) to handle
            // multi-line registrations like:
            //   register_rest_route(
            //       'namespace/v1',
            for caps in regex.captures_iter(&content) {
                if let Some(name_match) = caps.get(1) {
                    let name = name_match.as_str().to_string();
                    // Deduplicate: only count first occurrence of each feature name
                    if seen_names.contains(&name) {
                        continue;
                    }
                    seen_names.insert(name.clone());

                    let byte_pos = name_match.start();
                    let line_num = line_offsets.partition_point(|&offset| offset <= byte_pos);
                    let is_documented = all_doc_content.contains(&name);

                    // Extract context based on extension rules
                    let rule = context_rules
                        .iter()
                        .find(|(key, _)| pattern.contains(key.as_str()))
                        .map(|(_, r)| r);

                    let description = if rule.is_some_and(|r| r.doc_comment) {
                        extract_doc_comment(&content, byte_pos)
                    } else {
                        None
                    };

                    let fields = if rule.is_some_and(|r| r.block_fields) {
                        extract_block_fields(&content, byte_pos)
                    } else {
                        None
                    };

                    all_features.push(DetectedFeature {
                        name: name.clone(),
                        source_file: file.clone(),
                        line: line_num,
                        pattern: pattern.clone(),
                        documented: is_documented,
                        description,
                        fields,
                    });

                    if is_documented {
                        documented_count += 1;
                    } else {
                        undocumented.push(UndocumentedFeature {
                            name,
                            source_file: file.clone(),
                            line: line_num,
                            pattern: pattern.clone(),
                        });
                    }
                }
            }
        }
    }

    FeatureDetectionResult {
        total: seen_names.len(),
        documented: documented_count,
        undocumented,
        all_features,
    }
}

/// Directories to skip when scanning for source files.
const SKIP_DIRS: &[&str] = &[
    "vendor",
    "node_modules",
    ".git",
    "target",
    "build",
    "dist",
    "__pycache__",
    ".svn",
];

/// Recursively find all non-markdown source files, excluding common dependency dirs.
fn find_source_files(source_path: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_source_files(source_path, source_path, &mut files);
    files.sort();
    files
}

fn collect_source_files(base: &Path, dir: &Path, files: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect_source_files(base, &path, files);
        } else if path.is_file() && !name.ends_with(".md") {
            if let Ok(relative) = path.strip_prefix(base) {
                files.push(relative.to_string_lossy().to_string());
            }
        }
    }
}

/// Collect audit ignore patterns from all linked extensions.
pub(crate) fn collect_extension_ignore_patterns(comp: &component::Component) -> Vec<String> {
    let mut patterns = Vec::new();
    if let Some(ref extensions) = comp.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
                patterns.extend(manifest.audit_ignore_claim_patterns().to_vec());
            }
        }
    }
    patterns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diff_file_paths_basic() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
index abc123..def456 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
+// New comment
 fn main() {}
diff --git a/src/lib.rs b/src/lib.rs
index 111222..333444 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-old
+new
"#;
        let files = parse_diff_file_paths(diff);
        assert_eq!(files, vec!["src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn test_parse_diff_file_paths_empty() {
        let diff = "";
        let files = parse_diff_file_paths(diff);
        assert!(files.is_empty());
    }

    #[test]
    fn test_parse_diff_file_paths_no_diff_lines() {
        let diff = "Some random text\nwithout diff headers";
        let files = parse_diff_file_paths(diff);
        assert!(files.is_empty());
    }

    #[test]
    fn test_references_file_exact_match() {
        assert!(references_file("src/main.rs", "src/main.rs"));
        assert!(references_file("/src/main.rs", "src/main.rs"));
        assert!(references_file("src/main.rs", "/src/main.rs"));
    }

    #[test]
    fn test_references_file_directory_contains() {
        assert!(references_file("inc/Engine/", "inc/Engine/AI/Tools.php"));
        assert!(references_file("/inc/Engine/", "inc/Engine/AI/Tools.php"));
    }

    #[test]
    fn test_references_file_directory_no_match() {
        // Directory path should not match files outside it
        assert!(!references_file("inc/Engine/", "inc/Other/file.php"));
        // Non-directory paths should not use directory matching
        assert!(!references_file("inc/Engine", "inc/Engine/AI/Tools.php"));
    }

    #[test]
    fn test_references_file_basename_match() {
        // Code examples often reference class names without full paths
        assert!(references_file(
            "ToolExecutor::run()",
            "inc/Engine/ToolExecutor.php"
        ));
        assert!(references_file("new BaseTool()", "src/tools/BaseTool.rs"));
    }

    #[test]
    fn test_references_file_basename_short_name_no_match() {
        // Short names (< 4 chars) should not match to avoid false positives
        assert!(!references_file("use AI;", "src/AI.php"));
    }

    #[test]
    fn test_references_file_no_match() {
        assert!(!references_file("totally/different.rs", "src/main.rs"));
        assert!(!references_file("random text", "src/file.rs"));
    }

    #[test]
    fn test_build_doc_action_single_file() {
        let action = build_doc_action(&["src/main.rs".to_string()], 0);
        assert!(action.contains("src/main.rs"));
        assert!(action.contains("Source changed"));
    }

    #[test]
    fn test_build_doc_action_multiple_files() {
        let action = build_doc_action(&["src/main.rs".to_string(), "src/lib.rs".to_string()], 0);
        assert!(action.contains("2 files"));
    }

    #[test]
    fn test_build_doc_action_with_code_examples() {
        let action = build_doc_action(&["src/main.rs".to_string()], 3);
        assert!(action.contains("3 code example(s)"));
        assert!(action.contains("stale"));
    }

    #[test]
    fn test_extract_broken_references() {
        let tasks = vec![
            AuditTask {
                doc: "api/index.md".to_string(),
                line: 10,
                claim: "file path `src/old.rs`".to_string(),
                claim_type: ClaimType::FilePath,
                claim_value: "src/old.rs".to_string(),
                confidence: ClaimConfidence::Real,
                status: AuditTaskStatus::Broken,
                action: Some("File no longer exists".to_string()),
            },
            AuditTask {
                doc: "api/index.md".to_string(),
                line: 20,
                claim: "file path `src/main.rs`".to_string(),
                claim_type: ClaimType::FilePath,
                claim_value: "src/main.rs".to_string(),
                confidence: ClaimConfidence::Real,
                status: AuditTaskStatus::Verified,
                action: None,
            },
        ];

        let doc_contents = HashMap::new(); // No content for context extraction
        let broken = extract_broken_references(&tasks, &doc_contents);
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].doc, "api/index.md");
        assert_eq!(broken[0].line, 10);
        assert_eq!(broken[0].action, "File no longer exists");
        assert!(broken[0].doc_context.is_none()); // No content provided
    }

    #[test]
    fn test_build_priority_docs_filters_by_changed_files() {
        let tasks = vec![
            AuditTask {
                doc: "api/tools.md".to_string(),
                line: 10,
                claim: "file path `inc/ToolExecutor.php`".to_string(),
                claim_type: ClaimType::FilePath,
                claim_value: "inc/ToolExecutor.php".to_string(),
                confidence: ClaimConfidence::Real,
                status: AuditTaskStatus::Verified,
                action: None,
            },
            AuditTask {
                doc: "api/other.md".to_string(),
                line: 5,
                claim: "file path `inc/Unrelated.php`".to_string(),
                claim_type: ClaimType::FilePath,
                claim_value: "inc/Unrelated.php".to_string(),
                confidence: ClaimConfidence::Real,
                status: AuditTaskStatus::Verified,
                action: None,
            },
        ];

        let changed_files = vec!["inc/ToolExecutor.php".to_string()];
        let priority = build_priority_docs(&tasks, &changed_files);

        // Only api/tools.md should be a priority doc
        assert_eq!(priority.len(), 1);
        assert_eq!(priority[0].doc, "api/tools.md");
        assert_eq!(
            priority[0].changed_files_referenced,
            vec!["inc/ToolExecutor.php"]
        );
    }

    #[test]
    fn test_build_priority_docs_sorts_by_impact() {
        let tasks = vec![
            AuditTask {
                doc: "doc_one.md".to_string(),
                line: 1,
                claim_type: ClaimType::FilePath,
                claim: "".to_string(),
                claim_value: "file1.rs".to_string(),
                confidence: ClaimConfidence::Real,
                status: AuditTaskStatus::Verified,
                action: None,
            },
            AuditTask {
                doc: "doc_two.md".to_string(),
                line: 1,
                claim_type: ClaimType::FilePath,
                claim: "".to_string(),
                claim_value: "file1.rs".to_string(),
                confidence: ClaimConfidence::Real,
                status: AuditTaskStatus::Verified,
                action: None,
            },
            AuditTask {
                doc: "doc_two.md".to_string(),
                line: 2,
                claim_type: ClaimType::FilePath,
                claim: "".to_string(),
                claim_value: "file2.rs".to_string(),
                confidence: ClaimConfidence::Real,
                status: AuditTaskStatus::Verified,
                action: None,
            },
        ];

        let changed_files = vec!["file1.rs".to_string(), "file2.rs".to_string()];
        let priority = build_priority_docs(&tasks, &changed_files);

        // doc_two.md references 2 files, doc_one.md references 1
        // So doc_two.md should come first
        assert_eq!(priority.len(), 2);
        assert_eq!(priority[0].doc, "doc_two.md");
        assert_eq!(priority[0].changed_files_referenced.len(), 2);
        assert_eq!(priority[1].doc, "doc_one.md");
        assert_eq!(priority[1].changed_files_referenced.len(), 1);
    }

    #[test]
    fn test_detect_features_finds_missing() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        fs::create_dir_all(&docs_path).unwrap();

        // Create a source file with a feature registration
        fs::write(
            source_path.join("plugin.js"),
            "registerStepType('coolStep', handler);\nregisterStepType('docStep', handler);\n",
        )
        .unwrap();

        // Create a doc file that mentions docStep but not coolStep
        fs::write(
            docs_path.join("guide.md"),
            "Use the docStep to do things.\n",
        )
        .unwrap();

        let patterns = vec![r#"registerStepType\(\s*'(\w+)'"#.to_string()];

        let result = detect_features(
            &patterns,
            source_path,
            &["docs".to_string()],
            &[],
            &HashMap::new(),
        );

        assert_eq!(result.total, 2);
        assert_eq!(result.documented, 1);
        assert_eq!(result.undocumented.len(), 1);
        assert_eq!(result.undocumented[0].name, "coolStep");
        assert_eq!(result.undocumented[0].source_file, "plugin.js");
        assert_eq!(result.undocumented[0].line, 1);
    }

    #[test]
    fn test_detect_features_empty_when_no_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_features(&[], dir.path(), &["docs".to_string()], &[], &HashMap::new());
        assert_eq!(result.total, 0);
        assert!(result.undocumented.is_empty());
    }

    #[test]
    fn test_detect_features_skips_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        fs::create_dir_all(&docs_path).unwrap();

        fs::write(
            source_path.join("notes.md"),
            "registerStepType('hidden', h);\n",
        )
        .unwrap();

        let patterns = vec![r#"registerStepType\(\s*'(\w+)'"#.to_string()];

        let result = detect_features(
            &patterns,
            source_path,
            &["docs".to_string()],
            &[],
            &HashMap::new(),
        );
        assert_eq!(result.total, 0);
        assert!(result.undocumented.is_empty());
    }

    #[test]
    fn test_detect_features_all_documented() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        fs::create_dir_all(&docs_path).unwrap();

        fs::write(
            source_path.join("plugin.js"),
            "registerStepType('myStep', handler);\n",
        )
        .unwrap();
        fs::write(
            docs_path.join("guide.md"),
            "The myStep feature does things.\n",
        )
        .unwrap();

        let patterns = vec![r#"registerStepType\(\s*'(\w+)'"#.to_string()];

        let result = detect_features(
            &patterns,
            source_path,
            &["docs".to_string()],
            &[],
            &HashMap::new(),
        );
        assert_eq!(result.total, 1);
        assert_eq!(result.documented, 1);
        assert!(result.undocumented.is_empty());
    }

    #[test]
    fn test_detect_features_scans_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        let inc_path = source_path.join("inc").join("Api");
        fs::create_dir_all(&docs_path).unwrap();
        fs::create_dir_all(&inc_path).unwrap();

        // Feature in a nested subdirectory (not in changed files)
        fs::write(
            inc_path.join("Routes.php"),
            "register_rest_route('myplugin/v1', '/items', []);\n",
        )
        .unwrap();

        let patterns = vec![r#"register_rest_route\(\s*['"](\w[\w-]*/v\d+)['"]"#.to_string()];

        let result = detect_features(
            &patterns,
            source_path,
            &["docs".to_string()],
            &[],
            &HashMap::new(),
        );

        assert_eq!(result.undocumented.len(), 1);
        assert_eq!(result.undocumented[0].name, "myplugin/v1");
        assert!(result.undocumented[0].source_file.contains("Routes.php"));
    }

    #[test]
    fn test_detect_features_skips_vendor() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        let vendor_path = source_path.join("vendor").join("lib");
        fs::create_dir_all(&docs_path).unwrap();
        fs::create_dir_all(&vendor_path).unwrap();

        // Feature in vendor should be ignored
        fs::write(
            vendor_path.join("plugin.php"),
            "register_rest_route('vendor/v1', '/stuff', []);\n",
        )
        .unwrap();

        let patterns = vec![r#"register_rest_route\(\s*['"](\w[\w-]*/v\d+)['"]"#.to_string()];

        let result = detect_features(
            &patterns,
            source_path,
            &["docs".to_string()],
            &[],
            &HashMap::new(),
        );
        assert_eq!(result.total, 0);
        assert!(result.undocumented.is_empty());
    }

    #[test]
    fn test_detect_features_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        fs::create_dir_all(&docs_path).unwrap();

        // Same feature name registered in two files
        fs::write(
            source_path.join("a.js"),
            "registerStepType('myStep', handler);\n",
        )
        .unwrap();
        fs::write(
            source_path.join("b.js"),
            "registerStepType('myStep', handler);\n",
        )
        .unwrap();

        let patterns = vec![r#"registerStepType\(\s*'(\w+)'"#.to_string()];

        let result = detect_features(
            &patterns,
            source_path,
            &["docs".to_string()],
            &[],
            &HashMap::new(),
        );
        assert_eq!(result.total, 1); // Deduplicated
        assert_eq!(result.undocumented.len(), 1);
    }

    #[test]
    fn test_detect_features_reads_readme() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        fs::create_dir_all(&docs_path).unwrap();

        // Feature in source
        fs::write(
            source_path.join("plugin.js"),
            "registerStepType('readmeStep', handler);\nregisterStepType('hiddenStep', handler);\n",
        )
        .unwrap();

        // README.md documents one of them (no docs/ file does)
        fs::write(
            source_path.join("README.md"),
            "This plugin provides readmeStep for automation.\n",
        )
        .unwrap();

        let patterns = vec![r#"registerStepType\(\s*'(\w+)'"#.to_string()];
        let result = detect_features(
            &patterns,
            source_path,
            &["docs".to_string()],
            &[],
            &HashMap::new(),
        );

        // readmeStep is documented via README, hiddenStep is not
        assert_eq!(result.total, 2);
        assert_eq!(result.documented, 1);
        assert_eq!(result.undocumented.len(), 1);
        assert_eq!(result.undocumented[0].name, "hiddenStep");
    }

    #[test]
    fn test_detect_features_multiple_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        let wiki_path = source_path.join("wiki");
        fs::create_dir_all(&docs_path).unwrap();
        fs::create_dir_all(&wiki_path).unwrap();

        // Feature in source
        fs::write(
            source_path.join("plugin.js"),
            "registerStepType('wikiStep', handler);\nregisterStepType('orphanStep', handler);\n",
        )
        .unwrap();

        // Documented in wiki, not in docs
        fs::write(
            wiki_path.join("features.md"),
            "The wikiStep handles wiki operations.\n",
        )
        .unwrap();

        let patterns = vec![r#"registerStepType\(\s*'(\w+)'"#.to_string()];

        // Only scanning docs/ — both undocumented
        let result = detect_features(
            &patterns,
            source_path,
            &["docs".to_string()],
            &[],
            &HashMap::new(),
        );
        assert_eq!(result.total, 2);
        assert_eq!(result.undocumented.len(), 2);

        // Scanning both dirs — wikiStep found in wiki/
        let result = detect_features(
            &patterns,
            source_path,
            &["docs".to_string(), "wiki".to_string()],
            &[],
            &HashMap::new(),
        );
        assert_eq!(result.total, 2);
        assert_eq!(result.documented, 1);
        assert_eq!(result.undocumented.len(), 1);
        assert_eq!(result.undocumented[0].name, "orphanStep");
    }

    #[test]
    fn test_extract_doc_context_with_content() {
        let mut doc_contents = HashMap::new();
        doc_contents.insert(
            "api/tools.md".to_string(),
            "# Tools\n\nSee `src/old.rs` for details.\n\nMore content here.\n".to_string(),
        );

        // Line 3 is "See `src/old.rs` for details."
        let context = extract_doc_context(&doc_contents, "api/tools.md", 3);
        assert!(context.is_some());
        let lines = context.unwrap();
        assert_eq!(lines.len(), 3); // line 2, 3, 4
        assert!(lines[0].starts_with("2:"));
        assert!(lines[1].contains("src/old.rs"));
        assert!(lines[2].starts_with("4:"));
    }

    #[test]
    fn test_extract_doc_context_first_line() {
        let mut doc_contents = HashMap::new();
        doc_contents.insert("test.md".to_string(), "# Title\nSecond line\n".to_string());

        let context = extract_doc_context(&doc_contents, "test.md", 1);
        assert!(context.is_some());
        let lines = context.unwrap();
        assert_eq!(lines.len(), 2); // line 1, 2 (no line before)
        assert!(lines[0].starts_with("1:"));
    }

    #[test]
    fn test_extract_doc_context_missing_doc() {
        let doc_contents = HashMap::new();
        let context = extract_doc_context(&doc_contents, "nonexistent.md", 1);
        assert!(context.is_none());
    }

    #[test]
    fn test_broken_reference_includes_context() {
        let tasks = vec![AuditTask {
            doc: "api/tools.md".to_string(),
            line: 2,
            claim: "file path `src/old.rs`".to_string(),
            claim_type: ClaimType::FilePath,
            claim_value: "src/old.rs".to_string(),
            confidence: ClaimConfidence::Real,
            status: AuditTaskStatus::Broken,
            action: Some("File no longer exists".to_string()),
        }];

        let mut doc_contents = HashMap::new();
        doc_contents.insert(
            "api/tools.md".to_string(),
            "# API\nSee `src/old.rs` for the tool.\nMore info below.\n".to_string(),
        );

        let broken = extract_broken_references(&tasks, &doc_contents);
        assert_eq!(broken.len(), 1);
        assert!(broken[0].doc_context.is_some());
        let ctx = broken[0].doc_context.as_ref().unwrap();
        assert!(ctx.iter().any(|l| l.contains("src/old.rs")));
    }

    #[test]
    fn test_find_doc_files_excludes_configured_targets() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(
            docs_path.join("CHANGELOG.md"),
            "# Changelog\n## v1.0\n- Removed old/path.rs\n",
        )
        .unwrap();
        fs::write(docs_path.join("api.md"), "# API\n").unwrap();

        let files = find_doc_files(docs_path, &["CHANGELOG.md".to_string()]);
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"api.md".to_string()));
        assert!(files.contains(&"guide.md".to_string()));
        assert!(!files.iter().any(|f| f.to_lowercase().contains("changelog")));
    }

    #[test]
    fn test_find_doc_files_exclusion_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(docs_path.join("changelog.md"), "# Changes\n").unwrap();

        let files = find_doc_files(docs_path, &["CHANGELOG.md".to_string()]);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "guide.md");
    }

    #[test]
    fn test_find_doc_files_no_exclusion_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(docs_path.join("CHANGELOG.md"), "# Changelog\n").unwrap();

        // Without exclusion, changelog should be included
        let files = find_doc_files(docs_path, &[]);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f == "CHANGELOG.md"));
    }

    #[test]
    fn test_find_doc_files_custom_excluded_target() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(docs_path.join("CHANGELOG.md"), "# Changelog\n").unwrap();
        fs::write(docs_path.join("CHANGES.md"), "# Changes\n").unwrap();

        let files = find_doc_files(docs_path, &["CHANGES.md".to_string()]);
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"CHANGELOG.md".to_string()));
        assert!(files.contains(&"guide.md".to_string()));
        assert!(!files.iter().any(|f| f == "CHANGES.md"));
    }

    #[test]
    fn test_build_priority_docs_empty_when_no_changes() {
        let tasks = vec![AuditTask {
            doc: "api/tools.md".to_string(),
            line: 10,
            claim: "file path `inc/Tool.php`".to_string(),
            claim_type: ClaimType::FilePath,
            claim_value: "inc/Tool.php".to_string(),
            confidence: ClaimConfidence::Real,
            status: AuditTaskStatus::Verified,
            action: None,
        }];

        let changed_files: Vec<String> = vec![];
        let priority = build_priority_docs(&tasks, &changed_files);

        assert!(priority.is_empty());
    }
}
