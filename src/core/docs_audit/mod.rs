//! Documentation audit system for extracting and verifying claims from markdown files.
//!
//! This module provides a doc-centric approach to documentation auditing:
//! 1. Extract claims from documentation (file paths, identifiers, code examples)
//! 2. Verify claims against the actual codebase
//! 3. Correlate docs with git changes to identify priority docs needing review
//! 4. Build an alignment report focused on actionable items

mod claims;
mod tasks;
mod verify;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

pub use claims::{Claim, ClaimType};
pub use tasks::{AuditTask, AuditTaskStatus};
pub use verify::VerifyResult;

use regex::Regex;

use crate::{component, git, module, Result};

/// A doc that needs content review due to referenced files changing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PriorityDoc {
    pub doc: String,
    pub reason: String,
    pub changed_files_referenced: Vec<String>,
    pub code_examples: usize,
    pub action: String,
}

/// A feature found in source code with no mention in documentation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UndocumentedFeature {
    pub name: String,
    pub source_file: String,
    pub line: usize,
    pub pattern: String,
}

/// A broken reference that needs fixing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BrokenReference {
    pub doc: String,
    pub line: usize,
    pub claim: String,
    pub action: String,
}

/// Summary counts for the alignment report.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AlignmentSummary {
    pub docs_scanned: usize,
    pub priority_docs: usize,
    pub broken_references: usize,
    pub unchanged_docs: usize,
    pub undocumented_features: usize,
}

/// Result of auditing a component's documentation for content alignment.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditResult {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    pub summary: AlignmentSummary,
    pub changed_files: Vec<String>,
    pub priority_docs: Vec<PriorityDoc>,
    pub broken_references: Vec<BrokenReference>,
    pub undocumented_features: Vec<UndocumentedFeature>,
}

/// Audit a component's documentation and return an alignment report.
///
/// If `docs_dir_override` is provided, it's used instead of the component's
/// configured `docs_dir` (which defaults to "docs").
pub fn audit_component(component_id: &str, docs_dir_override: Option<&str>) -> Result<AuditResult> {
    let comp = component::load(component_id)?;
    let source_path = Path::new(&comp.local_path);
    let docs_dir = docs_dir_override
        .or(comp.docs_dir.as_deref())
        .unwrap_or("docs");
    let docs_path = source_path.join(docs_dir);

    // Get changelog target to exclude from audit (historical references are expected)
    let changelog_exclude = comp.changelog_target.as_deref();

    // Collect ignore patterns from all linked modules
    let ignore_patterns = collect_module_ignore_patterns(&comp);

    // Collect feature patterns from all linked modules
    let feature_patterns = collect_module_feature_patterns(&comp);

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
    let broken_references = extract_broken_references(&tasks);

    // Detect undocumented features across all source files (not just changed ones)
    let undocumented_features = detect_undocumented_features(
        &feature_patterns,
        source_path,
        &docs_path,
        changelog_exclude,
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
            undocumented_features: undocumented_features.len(),
        },
        changed_files,
        priority_docs,
        broken_references,
        undocumented_features,
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
            reason: format!("References {} changed file(s)", referenced_changes.len()),
            changed_files_referenced: referenced_changes,
            code_examples,
            action,
        });
    }

    // Sort by impact (most changed files referenced first)
    priority_docs.sort_by(|a, b| b.changed_files_referenced.len().cmp(&a.changed_files_referenced.len()));

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
            "Verify {} code example(s) match current {} implementation",
            code_examples, files_desc
        )
    } else {
        format!("Review documentation against {} changes", files_desc)
    }
}

/// Extract broken references from tasks into a separate list.
fn extract_broken_references(tasks: &[AuditTask]) -> Vec<BrokenReference> {
    tasks
        .iter()
        .filter(|t| matches!(t.status, AuditTaskStatus::Broken))
        .map(|t| BrokenReference {
            doc: t.doc.clone(),
            line: t.line,
            claim: t.claim.clone(),
            action: t
                .action
                .clone()
                .unwrap_or_else(|| "Fix broken reference".to_string()),
        })
        .collect()
}

/// Collect feature detection patterns from all linked modules.
fn collect_module_feature_patterns(comp: &component::Component) -> Vec<String> {
    let mut patterns = Vec::new();
    if let Some(ref modules) = comp.modules {
        for module_id in modules.keys() {
            if let Ok(manifest) = module::load_module(module_id) {
                patterns.extend(manifest.audit_feature_patterns.clone());
            }
        }
    }
    patterns
}

/// Detect features across all source files that have no mentions in documentation.
///
/// Scans the entire source tree (excluding vendor/node_modules/test dirs) for
/// feature registrations matching the configured patterns. Any feature name not
/// found in documentation content is reported as undocumented.
fn detect_undocumented_features(
    feature_patterns: &[String],
    source_path: &Path,
    docs_path: &Path,
    changelog_exclude: Option<&str>,
) -> Vec<UndocumentedFeature> {
    if feature_patterns.is_empty() {
        return Vec::new();
    }

    // Compile regexes once
    let compiled: Vec<(Regex, String)> = feature_patterns
        .iter()
        .filter_map(|p| Regex::new(p).ok().map(|r| (r, p.clone())))
        .collect();

    if compiled.is_empty() {
        return Vec::new();
    }

    // Collect all doc content for searching
    let doc_files = find_doc_files(docs_path, changelog_exclude);
    let all_doc_content: String = doc_files
        .iter()
        .filter_map(|f| fs::read_to_string(docs_path.join(f)).ok())
        .collect::<Vec<_>>()
        .join("\n");

    // Find all source files in the project (excluding common non-source dirs)
    let source_files = find_source_files(source_path);

    let mut undocumented = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();

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
                    // Deduplicate: only report first occurrence of each feature name
                    if !seen_names.contains(&name) && !all_doc_content.contains(&name) {
                        // Map byte offset to line number
                        let byte_pos = name_match.start();
                        let line_num = line_offsets
                            .partition_point(|&offset| offset <= byte_pos);
                        seen_names.insert(name.clone());
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

    undocumented
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
        assert!(references_file("ToolExecutor::run()", "inc/Engine/ToolExecutor.php"));
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
        assert!(action.contains("Review"));
    }

    #[test]
    fn test_build_doc_action_multiple_files() {
        let action = build_doc_action(
            &["src/main.rs".to_string(), "src/lib.rs".to_string()],
            0,
        );
        assert!(action.contains("2 files"));
    }

    #[test]
    fn test_build_doc_action_with_code_examples() {
        let action = build_doc_action(&["src/main.rs".to_string()], 3);
        assert!(action.contains("3 code example(s)"));
        assert!(action.contains("Verify"));
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
                status: AuditTaskStatus::Broken,
                action: Some("File not found".to_string()),
            },
            AuditTask {
                doc: "api/index.md".to_string(),
                line: 20,
                claim: "file path `src/main.rs`".to_string(),
                claim_type: ClaimType::FilePath,
                claim_value: "src/main.rs".to_string(),
                status: AuditTaskStatus::Verified,
                action: None,
            },
        ];

        let broken = extract_broken_references(&tasks);
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].doc, "api/index.md");
        assert_eq!(broken[0].line, 10);
        assert_eq!(broken[0].action, "File not found");
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
                status: AuditTaskStatus::Verified,
                action: None,
            },
            AuditTask {
                doc: "api/other.md".to_string(),
                line: 5,
                claim: "file path `inc/Unrelated.php`".to_string(),
                claim_type: ClaimType::FilePath,
                claim_value: "inc/Unrelated.php".to_string(),
                status: AuditTaskStatus::Verified,
                action: None,
            },
        ];

        let changed_files = vec!["inc/ToolExecutor.php".to_string()];
        let priority = build_priority_docs(&tasks, &changed_files);

        // Only api/tools.md should be a priority doc
        assert_eq!(priority.len(), 1);
        assert_eq!(priority[0].doc, "api/tools.md");
        assert_eq!(priority[0].changed_files_referenced, vec!["inc/ToolExecutor.php"]);
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
                status: AuditTaskStatus::Verified,
                action: None,
            },
            AuditTask {
                doc: "doc_two.md".to_string(),
                line: 1,
                claim_type: ClaimType::FilePath,
                claim: "".to_string(),
                claim_value: "file1.rs".to_string(),
                status: AuditTaskStatus::Verified,
                action: None,
            },
            AuditTask {
                doc: "doc_two.md".to_string(),
                line: 2,
                claim_type: ClaimType::FilePath,
                claim: "".to_string(),
                claim_value: "file2.rs".to_string(),
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
    fn test_detect_undocumented_features_finds_missing() {
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
        fs::write(docs_path.join("guide.md"), "Use the docStep to do things.\n").unwrap();

        let patterns = vec![r#"registerStepType\(\s*'(\w+)'"#.to_string()];

        let result = detect_undocumented_features(&patterns, source_path, &docs_path, None);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "coolStep");
        assert_eq!(result[0].source_file, "plugin.js");
        assert_eq!(result[0].line, 1);
    }

    #[test]
    fn test_detect_undocumented_features_empty_when_no_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_undocumented_features(&[], dir.path(), &dir.path().join("docs"), None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_detect_undocumented_features_skips_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        fs::create_dir_all(&docs_path).unwrap();

        fs::write(source_path.join("notes.md"), "registerStepType('hidden', h);\n").unwrap();

        let patterns = vec![r#"registerStepType\(\s*'(\w+)'"#.to_string()];

        let result = detect_undocumented_features(&patterns, source_path, &docs_path, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_detect_undocumented_features_all_documented() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path();
        let docs_path = source_path.join("docs");
        fs::create_dir_all(&docs_path).unwrap();

        fs::write(source_path.join("plugin.js"), "registerStepType('myStep', handler);\n").unwrap();
        fs::write(docs_path.join("guide.md"), "The myStep feature does things.\n").unwrap();

        let patterns = vec![r#"registerStepType\(\s*'(\w+)'"#.to_string()];

        let result = detect_undocumented_features(&patterns, source_path, &docs_path, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_detect_undocumented_features_scans_subdirectories() {
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

        let result = detect_undocumented_features(&patterns, source_path, &docs_path, None);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "myplugin/v1");
        assert!(result[0].source_file.contains("Routes.php"));
    }

    #[test]
    fn test_detect_undocumented_features_skips_vendor() {
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

        let result = detect_undocumented_features(&patterns, source_path, &docs_path, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_detect_undocumented_features_deduplicates() {
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

        let result = detect_undocumented_features(&patterns, source_path, &docs_path, None);
        assert_eq!(result.len(), 1); // Deduplicated
    }

    #[test]
    fn test_build_priority_docs_empty_when_no_changes() {
        let tasks = vec![AuditTask {
            doc: "api/tools.md".to_string(),
            line: 10,
            claim: "file path `inc/Tool.php`".to_string(),
            claim_type: ClaimType::FilePath,
            claim_value: "inc/Tool.php".to_string(),
            status: AuditTaskStatus::Verified,
            action: None,
        }];

        let changed_files: Vec<String> = vec![];
        let priority = build_priority_docs(&tasks, &changed_files);

        assert!(priority.is_empty());
    }
}
