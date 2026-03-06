use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::extension::{self, ParsedItem};
use crate::Result;

use super::move_items::MoveOptions;
use super::MoveResult;

#[derive(Debug, Clone, Serialize)]
pub struct DecomposePlan {
    pub file: String,
    pub strategy: String,
    pub audit_safe: bool,
    pub total_items: usize,
    pub groups: Vec<DecomposeGroup>,
    pub projected_audit_impact: DecomposeAuditImpact,
    pub checklist: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecomposeAuditImpact {
    pub estimated_new_files: usize,
    pub estimated_new_test_files: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recommended_test_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub likely_findings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecomposeGroup {
    pub name: String,
    pub suggested_target: String,
    pub item_names: Vec<String>,
}

pub fn build_plan(
    file: &str,
    root: &Path,
    strategy: &str,
    audit_safe: bool,
) -> Result<DecomposePlan> {
    if strategy != "grouped" {
        return Err(crate::Error::validation_invalid_argument(
            "strategy",
            format!("Unsupported strategy '{}'. Use: grouped", strategy),
            None,
            None,
        ));
    }

    let source_path = root.join(file);
    if !source_path.is_file() {
        return Err(crate::Error::validation_invalid_argument(
            "file",
            format!("Source file does not exist: {}", file),
            None,
            None,
        ));
    }

    let content = std::fs::read_to_string(&source_path)
        .map_err(|e| crate::Error::internal_io(e.to_string(), Some(format!("read {}", file))))?;

    let mut warnings = Vec::new();
    let items = parse_items(file, &content).unwrap_or_else(|| {
        warnings.push("No refactor parser available for file type; plan may be sparse".to_string());
        vec![]
    });
    let items = dedupe_parsed_items(items);

    let groups = group_items(file, &items, audit_safe);
    let projected_audit_impact = project_audit_impact(&groups, audit_safe);

    let checklist = vec![
        "Review grouping and target filenames".to_string(),
        "Review projected audit impact before applying".to_string(),
        "Apply grouped extraction in one deterministic pass (homeboy refactor decompose --write)"
            .to_string(),
        "Run cargo test and homeboy audit --changed-since origin/main".to_string(),
        if audit_safe {
            "Prefer include fragments (.inc) for low-friction audit ratchet".to_string()
        } else {
            "If creating new source modules, add matching tests for recommended test files"
                .to_string()
        },
    ];

    Ok(DecomposePlan {
        file: file.to_string(),
        strategy: strategy.to_string(),
        audit_safe,
        total_items: items.len(),
        groups,
        projected_audit_impact,
        checklist,
        warnings,
    })
}

pub fn apply_plan(plan: &DecomposePlan, root: &Path, write: bool) -> Result<Vec<MoveResult>> {
    let preview = run_moves(plan, root, false)?;
    if !write {
        return Ok(preview);
    }

    // Two-phase execution: validate first (dry-run), then apply.
    // This avoids partial writes from bad plans.
    run_moves(plan, root, true)
}

pub fn apply_plan_skeletons(plan: &DecomposePlan, root: &Path) -> Result<Vec<String>> {
    let mut created = Vec::new();

    for group in &plan.groups {
        let path = root.join(&group.suggested_target);
        if path.exists() {
            continue;
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::Error::internal_io(
                    e.to_string(),
                    Some(format!("create directory {}", parent.display())),
                )
            })?;
        }

        let header = format!(
            "// Decompose skeleton for group: {}\n// Planned items: {}\n\n",
            group.name,
            group.item_names.join(", ")
        );

        std::fs::write(&path, header).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
        })?;
        created.push(group.suggested_target.clone());
    }

    Ok(created)
}

fn run_moves(plan: &DecomposePlan, root: &Path, write: bool) -> Result<Vec<MoveResult>> {
    let mut results = Vec::new();

    for group in &plan.groups {
        let mut seen = HashSet::new();
        let deduped_item_names: Vec<&str> = group
            .item_names
            .iter()
            .filter_map(|name| {
                if seen.insert(name.clone()) {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect();

        let result = super::move_items::move_items_with_options(
            &deduped_item_names,
            &plan.file,
            &group.suggested_target,
            root,
            write,
            MoveOptions {
                move_related_tests: false,
            },
        )?;
        results.push(result);
    }

    Ok(results)
}

fn project_audit_impact(groups: &[DecomposeGroup], audit_safe: bool) -> DecomposeAuditImpact {
    let mut likely_findings = Vec::new();
    let mut recommended_test_files = Vec::new();

    if audit_safe {
        likely_findings.push(
            "Lower risk mode: include fragments usually avoid new module/test convention drift"
                .to_string(),
        );
    } else {
        for group in groups {
            if let Some(test_file) = source_to_test_file(&group.suggested_target) {
                recommended_test_files.push(test_file);
            }

            if group.suggested_target.starts_with("src/commands/")
                && group.suggested_target.ends_with(".rs")
            {
                likely_findings.push(format!(
                    "{} may trigger command convention checks (run method + command tests)",
                    group.suggested_target
                ));
            }
        }

        if !recommended_test_files.is_empty() {
            likely_findings.push(
                "New src/*.rs targets likely need matching tests to avoid MissingTestFile drift"
                    .to_string(),
            );
        }
    }

    DecomposeAuditImpact {
        estimated_new_files: groups.len(),
        estimated_new_test_files: recommended_test_files.len(),
        recommended_test_files,
        likely_findings,
    }
}

fn source_to_test_file(target: &str) -> Option<String> {
    if !target.starts_with("src/") || !target.ends_with(".rs") {
        return None;
    }

    let without_src = target.strip_prefix("src/")?;
    let without_ext = without_src.strip_suffix(".rs")?;
    Some(format!("tests/{}_test.rs", without_ext))
}

fn parse_items(file: &str, content: &str) -> Option<Vec<ParsedItem>> {
    let ext = Path::new(file).extension()?.to_str()?;
    let manifest = extension::find_extension_for_file_ext(ext, "refactor")?;
    let command = serde_json::json!({
        "command": "parse_items",
        "file_path": file,
        "content": content,
    });
    let result = extension::run_refactor_script(&manifest, &command)?;
    serde_json::from_value(result.get("items")?.clone()).ok()
}

fn group_items(file: &str, items: &[ParsedItem], audit_safe: bool) -> Vec<DecomposeGroup> {
    let source = PathBuf::from(file);
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .to_string();
    let base_dir = source
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for item in items {
        let bucket = match item.kind.as_str() {
            "struct" | "enum" | "trait" | "type_alias" | "impl" => "types",
            "const" | "static" => "constants",
            "function" => classify_function(&item.name),
            "test" => "tests",
            _ => "misc",
        };
        buckets
            .entry(bucket.to_string())
            .or_default()
            .push(item.name.clone());
    }

    for names in buckets.values_mut() {
        let mut seen = HashSet::new();
        names.retain(|name| seen.insert(name.clone()));
    }

    let ext = if audit_safe { "inc" } else { "rs" };

    buckets
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(group, names)| DecomposeGroup {
            suggested_target: if base_dir.is_empty() {
                format!("{}/{group}.{ext}", stem)
            } else {
                format!("{}/{}/{group}.{ext}", base_dir, stem)
            },
            name: group,
            item_names: names,
        })
        .collect()
}

fn dedupe_parsed_items(items: Vec<ParsedItem>) -> Vec<ParsedItem> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for item in items {
        let key = (
            item.kind.clone(),
            item.name.clone(),
            item.start_line,
            item.end_line,
        );

        if seen.insert(key) {
            deduped.push(item);
        }
    }

    deduped
}

fn classify_function(name: &str) -> &'static str {
    if name.starts_with("validate") || name.starts_with("check") {
        "validation"
    } else if name.starts_with("parse") || name.starts_with("resolve") {
        "planning"
    } else if name.starts_with("execute") || name.starts_with("deploy") || name == "run" {
        "execution"
    } else {
        "helpers"
    }
}
