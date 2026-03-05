use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::extension::{self, ParsedItem};
use crate::Result;

#[derive(Debug, Clone, Serialize)]
pub struct DecomposePlan {
    pub file: String,
    pub strategy: String,
    pub audit_safe: bool,
    pub total_items: usize,
    pub groups: Vec<DecomposeGroup>,
    pub checklist: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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

    let groups = group_items(file, &items, audit_safe);

    let checklist = vec![
        "Review grouping and target filenames".to_string(),
        "Apply move operations per group (homeboy refactor move)".to_string(),
        "Run cargo test and homeboy audit --changed-since origin/main".to_string(),
        if audit_safe {
            "Prefer include fragments (.inc) for low-friction audit ratchet".to_string()
        } else {
            "If creating new source modules, add/adjust matching tests".to_string()
        },
    ];

    Ok(DecomposePlan {
        file: file.to_string(),
        strategy: strategy.to_string(),
        audit_safe,
        total_items: items.len(),
        groups,
        checklist,
        warnings,
    })
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
