use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::extension::{self, grammar, grammar_items, ParsedItem};
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanValues};
use crate::core::Result;

use super::move_items::{MoveOptions, MoveResult};

mod grouping;

use grouping::{group_items, is_non_extractable_item, is_public_item_source};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposePlan {
    #[serde(flatten, default)]
    pub plan: HomeboyPlan,
    pub file: String,
    pub strategy: String,
    pub total_items: usize,
    pub groups: Vec<DecomposeGroup>,
    pub projected_audit_impact: DecomposeAuditImpact,
    pub checklist: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposeAuditImpact {
    pub estimated_new_files: usize,
    pub estimated_new_test_files: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recommended_test_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub likely_findings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecomposeGroup {
    pub name: String,
    pub suggested_target: String,
    pub item_names: Vec<String>,
}

pub fn build_plan(file: &str, root: &Path, strategy: &str) -> Result<DecomposePlan> {
    if strategy != "grouped" {
        return Err(crate::core::Error::validation_invalid_argument(
            "strategy",
            format!("Unsupported strategy '{}'. Use: grouped", strategy),
            None,
            None,
        ));
    }

    let source_path = root.join(file);
    if !source_path.is_file() {
        return Err(crate::core::Error::validation_invalid_argument(
            "file",
            format!("Source file does not exist: {}", file),
            None,
            None,
        ));
    }

    let content = std::fs::read_to_string(&source_path).map_err(|e| {
        crate::core::Error::internal_io(e.to_string(), Some(format!("read {}", file)))
    })?;

    let mut warnings = Vec::new();
    let items = parse_items(file, &content).unwrap_or_else(|| {
        warnings.push("No refactor parser available for file type; plan may be sparse".to_string());
        vec![]
    });
    let items = dedupe_parsed_items(items);
    let items = filter_extractable_items(file, &content, items, &mut warnings);

    let groups = group_items(file, &items, &content);
    let projected_audit_impact = project_audit_impact(&groups);

    let checklist = vec![
        "Review grouping and target filenames".to_string(),
        "Review projected audit impact before applying".to_string(),
        "Apply grouped extraction in one deterministic pass (homeboy refactor decompose --write)"
            .to_string(),
        "Run cargo test and homeboy audit --changed-since origin/main".to_string(),
    ];

    Ok(DecomposePlan {
        plan: decompose_homeboy_plan(file, strategy, items.len(), &groups, &warnings),
        file: file.to_string(),
        strategy: strategy.to_string(),
        total_items: items.len(),
        groups,
        projected_audit_impact,
        checklist,
        warnings,
    })
}

fn decompose_homeboy_plan(
    file: &str,
    strategy: &str,
    total_items: usize,
    groups: &[DecomposeGroup],
    warnings: &[String],
) -> HomeboyPlan {
    HomeboyPlan::builder_for_description(PlanKind::Refactor, file.to_string())
        .mode("decompose")
        .inputs(
            PlanValues::new()
                .string("file", file)
                .string("strategy", strategy)
                .number("total_items", total_items as u64),
        )
        .steps(groups.iter().map(decompose_group_step))
        .warnings(warnings.to_vec())
        .summarize()
        .build()
}

fn decompose_group_step(group: &DecomposeGroup) -> PlanStep {
    PlanStep::ready(
        format!("refactor.decompose.{}", group.name),
        "refactor.decompose.extract_group",
    )
    .label(format!(
        "Extract {} item(s) to {}",
        group.item_names.len(),
        group.suggested_target
    ))
    .scope(group.item_names.clone())
    .inputs(
        PlanValues::new()
            .string("group", group.name.clone())
            .string("suggested_target", group.suggested_target.clone())
            .json("item_names", &group.item_names)
            .json("group_payload", group),
    )
    .build()
}

impl DecomposePlan {
    pub fn planned_groups(&self) -> Vec<DecomposeGroup> {
        decompose_groups_from_plan(&self.plan)
    }
}

fn decompose_groups_from_plan(plan: &HomeboyPlan) -> Vec<DecomposeGroup> {
    plan.steps
        .iter()
        .filter_map(|step| step.input_as("group_payload"))
        .collect()
}

pub fn apply_plan(plan: &DecomposePlan, root: &Path, write: bool) -> Result<Vec<MoveResult>> {
    // Pre-write validation: check brace balance on all source files involved
    if write {
        validate_plan_sources(plan, root)?;
    }

    let preview = run_moves(plan, root, false)?;
    if !write {
        return Ok(preview);
    }

    // Two-phase execution: validate first (dry-run), then apply.
    // This avoids partial writes from bad plans.
    let results = run_moves(plan, root, true)?;

    // After all moves complete, generate module index (mod declarations + pub use
    // re-exports) in the source file. Without this, callers that imported from the
    // original module can't find the items that were moved to submodules.
    if results.iter().any(|r| r.applied) {
        generate_source_module_index(plan, root);
    }

    Ok(results)
}

/// Generate mod declarations and pub use re-exports in the source file after decompose.
///
/// The source file (now acting as mod.rs for its submodules) needs:
/// - `mod submodule;` declarations for each created submodule
/// - `pub use submodule::*;` re-exports so callers don't break
///
/// Delegates to the language extension's `generate_module_index` command for
/// language-specific syntax (Rust `pub use`, PHP `require_once`, etc.).
fn generate_source_module_index(plan: &DecomposePlan, root: &Path) {
    let source_path = root.join(&plan.file);

    // Read remaining content of the source file (items that weren't moved)
    let remaining_content = std::fs::read_to_string(&source_path).unwrap_or_default();

    // Build submodule entries from the plan groups
    let submodules: Vec<super::move_items::ModuleIndexEntry> = plan
        .planned_groups()
        .iter()
        .filter_map(|group| {
            // Derive module name from the target path
            let target = Path::new(&group.suggested_target);
            let stem = target.file_stem()?.to_str()?;
            Some(super::move_items::ModuleIndexEntry {
                name: stem.to_string(),
                pub_items: public_items_for_group(plan, group),
            })
        })
        .collect();

    if submodules.is_empty() {
        return;
    }

    // Remove use imports that would conflict with the new mod declarations.
    // When we add `mod grammar;`, any existing `use ...::grammar;` in the
    // remaining content would create "name defined multiple times" errors.
    let submodule_names: Vec<&str> = submodules.iter().map(|s| s.name.as_str()).collect();
    let cleaned_content = remove_conflicting_use_imports(&remaining_content, &submodule_names);

    if let Some(content) =
        super::move_items::ext_generate_module_index(&plan.file, &submodules, &cleaned_content)
    {
        if let Err(e) = std::fs::write(&source_path, content) {
            eprintln!(
                "Warning: failed to write module index to {}: {}",
                source_path.display(),
                e
            );
        }
    }
}

/// Remove `use` imports that would conflict with new `mod` declarations.
///
/// When decompose generates `mod foo;` + `pub use foo::*;`, any existing
/// `use some::path::foo;` in the remaining content introduces the name `foo`
/// twice. This function removes those conflicting imports.
fn remove_conflicting_use_imports(content: &str, submodule_names: &[&str]) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            // Only check use statements
            if !trimmed.starts_with("use ") && !trimmed.starts_with("pub use ") {
                return true;
            }
            // Check if this use statement brings a conflicting name into scope.
            // Patterns: `use path::name;` or `use path::name as _;`
            for name in submodule_names {
                // Simple tail import: `use something::name;`
                if trimmed.ends_with(&format!("::{};\n", name))
                    || trimmed.ends_with(&format!("::{};", name))
                {
                    return false;
                }
                // Grouped import containing the name: `use something::{name, other};`
                // Remove the whole line if it only imports the conflicting name,
                // otherwise leave it (partial removal is too complex for now).
                if trimmed.contains(&format!("::{{{}}}", name))
                    || trimmed.contains(&format!("{{ {} }}", name))
                {
                    return false;
                }
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn apply_plan_skeletons(plan: &DecomposePlan, root: &Path) -> Result<Vec<String>> {
    let mut created = Vec::new();

    for group in plan.planned_groups() {
        let path = root.join(&group.suggested_target);
        if path.exists() {
            continue;
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::core::Error::internal_io(
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
            crate::core::Error::internal_io(
                e.to_string(),
                Some(format!("write {}", path.display())),
            )
        })?;
        created.push(group.suggested_target);
    }

    Ok(created)
}

fn run_moves(plan: &DecomposePlan, root: &Path, write: bool) -> Result<Vec<MoveResult>> {
    let mut results = Vec::new();

    for group in plan.planned_groups() {
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
                // Decompose generates pub use re-exports in the source file,
                // so callers importing from the original module path still work.
                // Rewriting sibling imports would produce incorrect submodule paths.
                skip_caller_rewrites: true,
            },
        )?;
        results.push(result);
    }

    Ok(results)
}

fn project_audit_impact(groups: &[DecomposeGroup]) -> DecomposeAuditImpact {
    let mut likely_findings = Vec::new();
    let mut recommended_test_files = Vec::new();

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
            "New src/*.rs targets will need matching tests (autofix handles this)".to_string(),
        );
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

    // Prefer the language extension's structural parser when available.
    // For decomposition quality, the language-aware parser should be the
    // authoritative source of item boundaries/source extraction. The core
    // grammar parser remains the fallback for languages without a dedicated
    // refactor parser.
    if let Some(manifest) = extension::find_extension_for_file_ext(ext, "refactor") {
        // Try extension script first
        let command = serde_json::json!({
            "command": "parse_items",
            "file_path": file,
            "content": content,
        });
        if let Some(result) = extension::run_refactor_script(&manifest, &command) {
            if let Some(items) = result
                .get("items")
                .and_then(|value| serde_json::from_value::<Vec<ParsedItem>>(value.clone()).ok())
            {
                if !items.is_empty() {
                    return Some(items);
                }
            }
        }

        // Fall back to core grammar parser
        if let Some(ext_path) = &manifest.extension_path {
            let grammar = grammar::load_for_extension_path(Path::new(ext_path), ext);
            if let Some(grammar) = grammar {
                let items = grammar_items::parse_items(content, &grammar);
                if !items.is_empty() {
                    return Some(items.into_iter().map(ParsedItem::from).collect());
                }
            }
        }
    }

    None
}

/// Validate that parsed items have balanced braces before writing.
///
/// This prevents the kind of corruption that killed upgrade.rs — if the parser
/// produced items with unbalanced braces, we abort before writing anything.
fn validate_plan_sources(plan: &DecomposePlan, root: &Path) -> Result<()> {
    let source_path = root.join(&plan.file);
    let content = std::fs::read_to_string(&source_path).map_err(|e| {
        crate::core::Error::internal_io(e.to_string(), Some("pre-write validation".to_string()))
    })?;

    let ext = Path::new(&plan.file).extension().and_then(|e| e.to_str());
    let grammar = ext.and_then(|ext| {
        let manifest = extension::find_extension_for_file_ext(ext, "refactor")?;
        let ext_path = manifest.extension_path.as_deref()?;
        grammar::load_for_extension_path(Path::new(ext_path), ext)
    });

    if let Some(grammar) = grammar {
        // Re-parse and validate each item's source has balanced braces
        let items = grammar_items::parse_items(&content, &grammar);
        for item in &items {
            if !grammar_items::validate_brace_balance(&item.source, &grammar) {
                return Err(crate::core::Error::validation_invalid_argument(
                    "file",
                    format!(
                        "Pre-write validation failed: item '{}' (lines {}-{}) has unbalanced braces. \
                         Aborting to prevent file corruption.",
                        item.name, item.start_line, item.end_line
                    ),
                    None,
                    Some(vec![
                        "This usually means the parser misjudged item boundaries".to_string(),
                        "Try running without --write to inspect the plan first".to_string(),
                    ]),
                ));
            }
        }
    }

    Ok(())
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

fn filter_extractable_items(
    file: &str,
    content: &str,
    items: Vec<ParsedItem>,
    warnings: &mut Vec<String>,
) -> Vec<ParsedItem> {
    let mut filtered = Vec::new();

    for item in items {
        if is_non_extractable_item(file, content, &item) {
            warnings.push(format!(
                "Skipped non-extractable item '{}' ({} lines {}-{}) during decompose planning",
                item.name, item.kind, item.start_line, item.end_line
            ));
            continue;
        }
        filtered.push(item);
    }

    filtered
}

fn public_items_for_group(plan: &DecomposePlan, group: &DecomposeGroup) -> Vec<String> {
    let source_path = Path::new(&plan.file);
    let ext = source_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("rs");

    let root = Path::new(".");
    let content = std::fs::read_to_string(source_path).unwrap_or_default();
    let items = parse_items_for_group_export(ext, &content, &plan.file).unwrap_or_default();

    let group_names: HashSet<&str> = group.item_names.iter().map(|name| name.as_str()).collect();
    let mut pub_items: Vec<String> = items
        .into_iter()
        .filter(|item| group_names.contains(item.name.as_str()))
        .filter(|item| is_public_item_source(&item.source))
        .filter_map(|item| export_name_for_item(&item))
        .collect();

    pub_items.sort();
    pub_items.dedup();

    // Fallback for cases where the source file has already been rewritten and
    // we can't recover the exact public names yet. Better to re-export the named
    // group items than spray a glob import.
    if pub_items.is_empty() {
        pub_items = group
            .item_names
            .iter()
            .filter(|name| !name.contains(" for "))
            .cloned()
            .collect();
    }

    let _ = root;
    pub_items
}

fn parse_items_for_group_export(ext: &str, content: &str, file: &str) -> Option<Vec<ParsedItem>> {
    let manifest = crate::core::extension::find_extension_for_file_ext(ext, "refactor")?;
    crate::core::refactor::move_items::ext_parse_items(&manifest, content, file)
        .or_else(|| crate::core::refactor::move_items::core_parse_items(&manifest, content))
}

fn export_name_for_item(item: &ParsedItem) -> Option<String> {
    match item.kind.as_str() {
        "function" | "struct" | "enum" | "trait" | "type_alias" | "const" | "static" => {
            Some(item.name.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use super::grouping::*;
    use super::*;

    fn item(name: &str, kind: &str) -> ParsedItem {
        ParsedItem {
            name: name.to_string(),
            kind: kind.to_string(),
            start_line: 1,
            end_line: 10,
            source: String::new(),
            visibility: String::new(),
        }
    }

    #[test]
    fn planned_groups_are_projected_from_homeboy_plan_steps() {
        let groups = vec![DecomposeGroup {
            name: "helpers".to_string(),
            suggested_target: "src/helpers.rs".to_string(),
            item_names: vec!["format_label".to_string()],
        }];
        let plan = DecomposePlan {
            plan: decompose_homeboy_plan("src/lib.rs", "grouped", 1, &groups, &[]),
            file: "src/lib.rs".to_string(),
            strategy: "grouped".to_string(),
            total_items: 1,
            groups: groups.clone(),
            projected_audit_impact: DecomposeAuditImpact {
                estimated_new_files: 1,
                estimated_new_test_files: 0,
                recommended_test_files: Vec::new(),
                likely_findings: Vec::new(),
            },
            checklist: Vec::new(),
            warnings: Vec::new(),
        };

        assert_eq!(plan.planned_groups(), groups);
    }

    #[test]
    fn cluster_by_name_segments_groups_shared_prefixes() {
        let names = vec![
            "extract_alpha_signatures",
            "extract_beta_signatures",
            "extract_gamma_signatures",
            "generate_stub",
            "generate_import",
            "generate_test",
            "validate_input",
        ];
        let clusters = cluster_by_name_segments(&names);

        // Should find clusters that group the extract_* and generate_* functions
        // The cluster name might be "extract", "signatures", "generate", etc.
        // depending on which segment is chosen as most specific
        let _extract_fns: Vec<&&str> = names[0..3].iter().collect();
        let _generate_fns: Vec<&&str> = names[3..6].iter().collect();

        // All 3 extract_* functions should be in the same cluster
        let extract_cluster = clusters
            .iter()
            .find(|(_, items)| items.contains(&"extract_alpha_signatures"));
        assert!(
            extract_cluster.is_some(),
            "extract_* functions should be clustered together"
        );
        let extract_items = &extract_cluster.unwrap().1;
        assert!(extract_items.contains(&"extract_beta_signatures"));
        assert!(extract_items.contains(&"extract_gamma_signatures"));

        // All 3 generate_* functions should be in the same cluster
        let generate_cluster = clusters
            .iter()
            .find(|(_, items)| items.contains(&"generate_stub"));
        assert!(
            generate_cluster.is_some(),
            "generate_* functions should be clustered together"
        );
        let generate_items = &generate_cluster.unwrap().1;
        assert!(generate_items.contains(&"generate_import"));
        assert!(generate_items.contains(&"generate_test"));
    }

    #[test]
    fn cluster_by_name_segments_unclustered_go_to_helpers() {
        let names = vec!["foo", "bar", "baz", "extract_a", "extract_b", "extract_c"];
        let clusters = cluster_by_name_segments(&names);

        let helpers = clusters.iter().find(|(name, _)| name == "helpers");
        assert!(helpers.is_some(), "Unclustered items should go to helpers");
        assert_eq!(helpers.unwrap().1.len(), 3); // foo, bar, baz
    }

    #[test]
    fn group_items_separates_types_from_functions() {
        let items = vec![
            item("Config", "struct"),
            item("Config", "impl"),
            item("Error", "enum"),
            item("load_config", "function"),
            item("save_config", "function"),
            item("validate_config", "function"),
        ];

        let groups = group_items("src/core/module.rs", &items, "");

        // Types and functions should be in separate groups
        let type_group = groups
            .iter()
            .find(|g| g.item_names.iter().any(|n| n == "Config" || n == "Error"));
        let fn_group = groups
            .iter()
            .find(|g| g.item_names.iter().any(|n| n == "load_config"));

        assert!(type_group.is_some(), "Should have a type group");
        assert!(fn_group.is_some(), "Should have a function group");

        // Types should not be in the function group
        let fn_group = fn_group.unwrap();
        assert!(
            !fn_group.item_names.contains(&"Config".to_string()),
            "Types should not leak into function groups"
        );
    }

    #[test]
    fn colocate_types_single_type() {
        let items = [item("Foo", "struct"), item("Foo", "impl")];
        let refs: Vec<&ParsedItem> = items.iter().collect();
        let groups = colocate_types(&refs);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "types");
        assert_eq!(groups[0].1.len(), 2);
    }

    #[test]
    fn colocate_types_multiple_types() {
        let items = [
            item("Foo", "struct"),
            item("Foo", "impl"),
            item("Bar", "enum"),
            item("Display for Foo", "impl"),
        ];
        let refs: Vec<&ParsedItem> = items.iter().collect();
        let groups = colocate_types(&refs);

        // Should have separate groups for Foo and Bar
        assert!(groups.len() >= 2);

        let foo_group = groups
            .iter()
            .find(|(_, names)| names.contains(&"Foo".to_string()));
        assert!(foo_group.is_some());
        let foo_names = &foo_group.unwrap().1;
        assert!(
            foo_names.contains(&"Display for Foo".to_string()),
            "Trait impl should be co-located with the type"
        );
    }

    #[test]
    fn split_oversized_group_produces_subclusters() {
        let names: Vec<String> = (0..20)
            .map(|i| {
                if i < 7 {
                    format!("extract_item_{}", i)
                } else if i < 14 {
                    format!("generate_stub_{}", i)
                } else {
                    format!("helper_{}", i)
                }
            })
            .collect();

        let groups = split_oversized_group("big_group", &names);
        assert!(
            groups.len() > 1,
            "Should split into multiple sub-clusters, got {}",
            groups.len()
        );
    }

    #[test]
    fn to_snake_case_converts_pascal() {
        assert_eq!(to_snake_case("FixKind"), "fix_kind");
        assert_eq!(to_snake_case("PreflightReport"), "preflight_report");
        assert_eq!(to_snake_case("Fix"), "fix");
        assert_eq!(to_snake_case("ApplyChunkResult"), "apply_chunk_result");
    }

    #[test]
    fn stop_words_are_filtered() {
        assert!(is_stop_word("get"));
        assert!(is_stop_word("set"));
        assert!(is_stop_word("is"));
        assert!(is_stop_word("from"));
        assert!(!is_stop_word("extract"));
        assert!(!is_stop_word("generate"));
        assert!(!is_stop_word("validate"));
    }

    #[test]
    fn merge_small_groups_consolidates_tiny_groups() {
        let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        buckets.insert(
            "big_group".to_string(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
        );
        buckets.insert("tiny".to_string(), vec!["x".to_string()]); // below threshold

        let merged = merge_small_groups(buckets);

        assert!(!merged.contains_key("tiny"), "Tiny group should be merged");
        assert!(
            merged.get("big_group").unwrap().contains(&"x".to_string()),
            "Tiny group items should be in the largest group"
        );
    }

    #[test]
    fn group_items_target_paths_use_file_stem() {
        let items = vec![
            item("foo", "function"),
            item("bar", "function"),
            item("baz", "function"),
        ];

        let groups = group_items("src/core/my_module.rs", &items, "");
        for g in &groups {
            assert!(
                g.suggested_target.starts_with("src/core/my_module/"),
                "Target should use file stem as directory: {}",
                g.suggested_target
            );
            assert!(
                g.suggested_target.ends_with(".rs"),
                "Non-audit-safe should use .rs extension"
            );
        }
    }

    #[test]
    fn group_items_preserves_source_extension() {
        let items = vec![
            item("foo", "function"),
            item("bar", "function"),
            item("baz", "function"),
        ];

        let groups = group_items("src/core/big.rs", &items, "");
        for g in &groups {
            assert!(
                g.suggested_target.ends_with(".rs"),
                "Should preserve .rs extension: {}",
                g.suggested_target
            );
        }
    }

    #[test]
    fn filter_extractable_items_skips_macro_template_fragments() {
        let content = r#"
macro_rules! entity_crud {
    ($Entity:ty $(; $($feature:ident),+ )?) => {
        pub fn load(id: &str) -> Result<$Entity> {
            config::load::<$Entity>(id)
        }
    };
}
"#;
        let items = vec![ParsedItem {
            name: "load".to_string(),
            kind: "function".to_string(),
            start_line: 3,
            end_line: 5,
            source:
                "pub fn load(id: &str) -> Result<$Entity> {\n    config::load::<$Entity>(id)\n}"
                    .to_string(),
            visibility: String::new(),
        }];
        let mut warnings = Vec::new();
        let filtered =
            filter_extractable_items("src/core/config.rs", content, items, &mut warnings);
        assert!(
            filtered.is_empty(),
            "macro template fragment should not be extractable"
        );
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn starts_like_extractable_declaration_rejects_dangling_body_fragment() {
        let source = ".map(|c| {\n    c.to_ascii_lowercase()\n})\n\npub fn slugify(s: &str) -> String {\n    s.to_string()\n}";
        assert!(!starts_like_extractable_declaration(source, "function"));
    }

    #[test]
    fn starts_like_extractable_declaration_accepts_attr_prefixed_function() {
        let source = "#[cfg(test)]\npub fn slugify(s: &str) -> String {\n    s.to_string()\n}";
        assert!(starts_like_extractable_declaration(source, "function"));
    }

    #[test]
    fn identify_parent_kept_functions_keeps_root_orchestrator() {
        let content = r#"
pub fn audit_component(component_id: &str) -> Result<CodeAuditResult> {
    audit_path_with_id(component_id, ".")
}

fn audit_path_with_id(component_id: &str, source_path: &str) -> Result<CodeAuditResult> {
    audit_internal(component_id, source_path)
}

fn audit_internal(component_id: &str, source_path: &str) -> Result<CodeAuditResult> {
    let _ = (component_id, source_path);
    Ok(todo!())
}
"#;
        let items = [ParsedItem {
                name: "audit_component".to_string(),
                kind: "function".to_string(),
                start_line: 2,
                end_line: 4,
                source: "pub fn audit_component(component_id: &str) -> Result<CodeAuditResult> {\n    audit_path_with_id(component_id, \".\")\n}".to_string(),
                visibility: "pub".to_string(),
            },
            ParsedItem {
                name: "audit_path_with_id".to_string(),
                kind: "function".to_string(),
                start_line: 6,
                end_line: 8,
                source: "fn audit_path_with_id(component_id: &str, source_path: &str) -> Result<CodeAuditResult> {\n    audit_internal(component_id, source_path)\n}".to_string(),
                visibility: String::new(),
            },
            ParsedItem {
                name: "audit_internal".to_string(),
                kind: "function".to_string(),
                start_line: 10,
                end_line: 13,
                source: "fn audit_internal(component_id: &str, source_path: &str) -> Result<CodeAuditResult> {\n    let _ = (component_id, source_path);\n    Ok(todo!())\n}".to_string(),
                visibility: String::new(),
            }];
        let refs: Vec<&ParsedItem> = items.iter().collect();
        let kept = identify_parent_kept_functions("src/core/code_audit/mod.rs", &refs, content);
        assert!(kept.contains("audit_component"));
        assert!(kept.contains("audit_internal"));
    }

    #[test]
    fn effective_module_root_detects_public_surface_regular_file() {
        let content = r#"
pub fn load() {}
pub fn list() {}
pub fn save() {}
pub fn delete() {}
"#;
        assert!(has_established_module_surface(content));
        assert!(is_effective_module_root("src/core/config.rs", content));
    }

    #[test]
    fn rebalance_for_viable_parent_surface_preserves_public_root_surface() {
        let content = r#"
pub fn a() {}
pub fn b() {}
fn helper() {}
"#;
        let items = [
            ParsedItem {
                name: "a".to_string(),
                kind: "function".to_string(),
                start_line: 1,
                end_line: 1,
                source: "pub fn a() {}".to_string(),
                visibility: "pub".to_string(),
            },
            ParsedItem {
                name: "b".to_string(),
                kind: "function".to_string(),
                start_line: 2,
                end_line: 2,
                source: "pub fn b() {}".to_string(),
                visibility: "pub".to_string(),
            },
            ParsedItem {
                name: "helper".to_string(),
                kind: "function".to_string(),
                start_line: 3,
                end_line: 3,
                source: "fn helper() {}".to_string(),
                visibility: String::new(),
            },
        ];
        let refs: Vec<&ParsedItem> = items.iter().collect();
        let mut buckets = BTreeMap::new();
        buckets.insert("alpha".to_string(), vec!["a".to_string()]);
        buckets.insert(
            "beta".to_string(),
            vec!["b".to_string(), "helper".to_string()],
        );

        let rebalanced =
            rebalance_for_viable_parent_surface("src/core/config.rs", content, &refs, buckets);
        assert!(rebalanced.contains_key("parent_surface"));
        assert_eq!(rebalanced["parent_surface"].len(), 2);
    }

    #[test]
    fn extract_sections_from_separator_headers() {
        let content = r#"
use something;

// ============================================================================
// Models
// ============================================================================

pub struct Foo {}

// ============================================================================
// Git operations
// ============================================================================

fn git_fetch() {}

// ============================================================================
// Diff parsing
// ============================================================================

fn parse_diff() {}
"#;
        let sections = extract_sections(content);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name, "models");
        assert_eq!(sections[1].name, "git_operations");
        assert_eq!(sections[2].name, "diff_parsing");
    }

    #[test]
    fn extract_sections_from_inline_headers() {
        let content = r#"
// === Types ===
struct A {}

// === Parsing ===
fn parse() {}

// === Rendering ===
fn render() {}
"#;
        let sections = extract_sections(content);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name, "types");
        assert_eq!(sections[1].name, "parsing");
        assert_eq!(sections[2].name, "rendering");
    }

    #[test]
    fn section_headers_guide_function_grouping() {
        let content = r#"
// ============================================================================
// Git operations
// ============================================================================

fn get_changed_files() {}
fn get_renamed_files() {}

// ============================================================================
// Diff parsing
// ============================================================================

fn extract_changes_from_diff() {}
fn parse_hunk() {}
"#;
        let items = vec![
            item_at("get_changed_files", "function", 5, 5),
            item_at("get_renamed_files", "function", 6, 6),
            item_at("extract_changes_from_diff", "function", 12, 12),
            item_at("parse_hunk", "function", 13, 13),
        ];

        let groups = group_items("src/core/drift.rs", &items, content);

        let git_group = groups
            .iter()
            .find(|g| g.item_names.contains(&"get_changed_files".to_string()));
        assert!(git_group.is_some(), "Should have a git group");
        let git_items = &git_group.unwrap().item_names;
        assert!(
            git_items.contains(&"get_renamed_files".to_string()),
            "Git functions should be in same section group"
        );

        let diff_group = groups.iter().find(|g| {
            g.item_names
                .contains(&"extract_changes_from_diff".to_string())
        });
        assert!(diff_group.is_some(), "Should have a diff group");
        let diff_items = &diff_group.unwrap().item_names;
        assert!(
            diff_items.contains(&"parse_hunk".to_string()),
            "Diff functions should be in same section group"
        );

        // The two groups should be different
        assert_ne!(
            git_group.unwrap().name,
            diff_group.unwrap().name,
            "Git and diff groups should be separate"
        );
    }

    fn item_at(name: &str, kind: &str, start: usize, end: usize) -> ParsedItem {
        ParsedItem {
            name: name.to_string(),
            kind: kind.to_string(),
            start_line: start,
            end_line: end,
            source: String::new(),
            visibility: String::new(),
        }
    }

    fn item_with_source(name: &str, kind: &str, source: &str) -> ParsedItem {
        ParsedItem {
            name: name.to_string(),
            kind: kind.to_string(),
            start_line: 1,
            end_line: 10,
            source: source.to_string(),
            visibility: String::new(),
        }
    }

    #[test]
    fn call_graph_clusters_related_functions() {
        let items: Vec<ParsedItem> = vec![
            item_with_source(
                "detect_drift",
                "function",
                "fn detect_drift() { get_changed_files(); extract_changes_from_diff(); }",
            ),
            item_with_source("get_changed_files", "function", "fn get_changed_files() {}"),
            item_with_source(
                "extract_changes_from_diff",
                "function",
                "fn extract_changes_from_diff() {}",
            ),
            item_with_source(
                "generate_rules",
                "function",
                "fn generate_rules() { is_auto_fixable(); }",
            ),
            item_with_source("is_auto_fixable", "function", "fn is_auto_fixable() {}"),
        ];
        let item_refs: Vec<&ParsedItem> = items.iter().collect();
        let fn_names: HashSet<&str> = items.iter().map(|i| i.name.as_str()).collect();

        let graph = build_call_graph(&item_refs, &fn_names);
        let (components, hubs) = call_graph_components(&graph);

        // detect_drift only calls 2 functions, below HUB_THRESHOLD (4),
        // so it should NOT be a hub — it clusters with its callees
        assert!(
            !hubs.contains(&"detect_drift".to_string()),
            "detect_drift calls only 2 functions, should not be a hub"
        );

        // detect_drift calls get_changed_files and extract_changes_from_diff → one component
        let detect_component = components
            .iter()
            .find(|(_, members)| members.contains(&"detect_drift".to_string()));
        assert!(
            detect_component.is_some(),
            "detect_drift group should exist"
        );
        let members = &detect_component.unwrap().1;
        assert!(members.contains(&"get_changed_files".to_string()));
        assert!(members.contains(&"extract_changes_from_diff".to_string()));

        // generate_rules calls is_auto_fixable → separate component
        let rules_component = components
            .iter()
            .find(|(_, members)| members.contains(&"generate_rules".to_string()));
        assert!(rules_component.is_some(), "rules group should exist");
        let members = &rules_component.unwrap().1;
        assert!(members.contains(&"is_auto_fixable".to_string()));
    }

    #[test]
    fn call_graph_excludes_hubs_from_clusters() {
        // An orchestrator function that calls 5+ others should be identified as a hub
        // and excluded from union-find to prevent mega-clusters
        let items: Vec<ParsedItem> = vec![
            item_with_source(
                "orchestrate",
                "function",
                "fn orchestrate() { step_a(); step_b(); step_c(); step_d(); step_e(); }",
            ),
            item_with_source("step_a", "function", "fn step_a() { helper_a(); }"),
            item_with_source("helper_a", "function", "fn helper_a() {}"),
            item_with_source("step_b", "function", "fn step_b() {}"),
            item_with_source("step_c", "function", "fn step_c() {}"),
            item_with_source("step_d", "function", "fn step_d() {}"),
            item_with_source("step_e", "function", "fn step_e() {}"),
        ];
        let item_refs: Vec<&ParsedItem> = items.iter().collect();
        let fn_names: HashSet<&str> = items.iter().map(|i| i.name.as_str()).collect();

        let graph = build_call_graph(&item_refs, &fn_names);
        let (components, hubs) = call_graph_components(&graph);

        // orchestrate calls 5 functions → should be a hub
        assert!(
            hubs.contains(&"orchestrate".to_string()),
            "orchestrate should be identified as a hub (calls {} functions)",
            graph.get("orchestrate").map(|c| c.len()).unwrap_or(0)
        );

        // step_a and helper_a should cluster together (step_a calls helper_a)
        let step_a_component = components
            .iter()
            .find(|(_, members)| members.contains(&"step_a".to_string()));
        assert!(
            step_a_component.is_some(),
            "step_a + helper_a should form a cluster"
        );
        assert!(step_a_component
            .unwrap()
            .1
            .contains(&"helper_a".to_string()));

        // Without hub exclusion, all 7 functions would be in one mega-component.
        // With hub exclusion, we should have smaller, focused clusters.
        let max_cluster_size = components.iter().map(|(_, m)| m.len()).max().unwrap_or(0);
        assert!(
            max_cluster_size < 6,
            "No cluster should contain all non-hub functions (max: {})",
            max_cluster_size
        );
    }

    #[test]
    fn find_dominant_prefix_detects_shared_naming() {
        let members = vec![
            "resolve_assertion".to_string(),
            "resolve_constructor".to_string(),
            "resolve_type_default".to_string(),
        ];
        let prefix = find_dominant_prefix(&members);
        assert_eq!(prefix, Some("resolve".to_string()));

        let members = vec![
            "infer_setup_from_condition".to_string(),
            "infer_hint_for_param".to_string(),
            "infer_setup_with_complements".to_string(),
        ];
        let prefix = find_dominant_prefix(&members);
        // 2/3 members share "infer_setup" — more specific than "infer"
        assert_eq!(prefix, Some("infer_setup".to_string()));

        // No dominant prefix
        let members = vec!["foo".to_string(), "bar".to_string(), "baz".to_string()];
        let prefix = find_dominant_prefix(&members);
        assert_eq!(prefix, None);
    }

    #[test]
    fn section_name_to_slug_converts_headers() {
        assert_eq!(section_name_to_slug("Models"), "models");
        assert_eq!(section_name_to_slug("Git operations"), "git_operations");
        // Long headers are truncated to MAX_MODULE_NAME_WORDS meaningful words
        assert_eq!(
            section_name_to_slug("Diff parsing — extract structural changes"),
            "diff_parsing_extract"
        );
        assert_eq!(section_name_to_slug("Tests"), "tests");
    }

    #[test]
    fn section_name_to_slug_converts_hyphens_to_underscores() {
        // Hyphens are invalid in Rust module names
        assert_eq!(section_name_to_slug("Whole-file move"), "whole_file_move");
        assert_eq!(
            section_name_to_slug("re-export handling"),
            "re_export_handling"
        );
        assert_eq!(section_name_to_slug("pre-commit hooks"), "pre_commit_hooks");
    }

    #[test]
    fn sanitize_module_name_handles_invalid_chars() {
        assert_eq!(sanitize_module_name("whole-file_move"), "whole_file_move");
        assert_eq!(sanitize_module_name("foo.bar"), "foo_bar");
        assert_eq!(sanitize_module_name("valid_name"), "valid_name");
        assert_eq!(sanitize_module_name("a--b"), "a_b");
        assert_eq!(sanitize_module_name("types"), "types");
    }

    #[test]
    fn name_prefixes_generates_multi_word() {
        let prefixes = name_prefixes("extract_changes_from_diff");
        assert!(prefixes.contains(&"extract_changes".to_string()));
        assert!(prefixes.contains(&"extract".to_string()));

        let prefixes = name_prefixes("foo");
        assert!(prefixes.contains(&"foo".to_string()));
        assert_eq!(prefixes.len(), 1);
    }

    #[test]
    fn cluster_with_min_size_two() {
        // With MIN_CLUSTER_SIZE=2, even pairs should cluster
        let names = vec![
            "parse_header",
            "parse_body",
            "render_output",
            "validate_input",
        ];
        let clusters = cluster_by_name_segments(&names);

        let parse_cluster = clusters
            .iter()
            .find(|(_, items)| items.contains(&"parse_header"));
        assert!(
            parse_cluster.is_some(),
            "parse_* pair should cluster together"
        );
        assert!(parse_cluster.unwrap().1.contains(&"parse_body"));
    }

    #[test]
    fn group_items_mod_rs_uses_parent_dir_not_mod_subdir() {
        // When source is mod.rs, submodules should go in the same directory,
        // not in a "mod/" subdirectory. This is how Rust module resolution works.
        let items = vec![
            item("foo", "function"),
            item("bar", "function"),
            item("baz", "function"),
        ];

        let groups = group_items("src/core/code_audit/mod.rs", &items, "");
        for g in &groups {
            assert!(
                g.suggested_target.starts_with("src/core/code_audit/"),
                "Target should be in parent dir, not mod/ subdir: {}",
                g.suggested_target
            );
            assert!(
                !g.suggested_target.contains("/mod/"),
                "Target must NOT contain /mod/ directory: {}",
                g.suggested_target
            );
            assert!(
                g.suggested_target.ends_with(".rs"),
                "Should have .rs extension: {}",
                g.suggested_target
            );
        }
    }

    #[test]
    fn group_items_regular_file_uses_stem_subdir() {
        // Regular files (not mod.rs) should use the stem as a subdirectory
        let items = vec![
            item("foo", "function"),
            item("bar", "function"),
            item("baz", "function"),
        ];

        let groups = group_items("src/core/operations.rs", &items, "");
        for g in &groups {
            assert!(
                g.suggested_target.starts_with("src/core/operations/"),
                "Regular file should use stem as subdir: {}",
                g.suggested_target
            );
        }
    }

    #[test]
    fn truncate_module_name_limits_word_count() {
        // Verbose section headers should be truncated
        assert_eq!(
            truncate_module_name("structural_parser_context_aware_iteration_over_source_text"),
            "structural_parser_context"
        );
        assert_eq!(
            truncate_module_name("grammar_definition_loaded_from_extension_toml_json"),
            "grammar_definition_loaded"
        );
        assert_eq!(
            truncate_module_name("extraction_apply_grammar_patterns_to_get_symbols"),
            "extraction_apply_grammar"
        );
        assert_eq!(
            truncate_module_name("convenience_helpers_for_feature_consumers"),
            "convenience_helpers_feature"
        );
    }

    #[test]
    fn truncate_module_name_preserves_short_names() {
        assert_eq!(truncate_module_name("types"), "types");
        assert_eq!(truncate_module_name("block_syntax"), "block_syntax");
        assert_eq!(truncate_module_name("grammar_loading"), "grammar_loading");
        assert_eq!(truncate_module_name("symbol"), "symbol");
    }

    #[test]
    fn truncate_module_name_drops_stop_words() {
        // Stop words like "for", "from", "to", "in" are dropped, not counted
        assert_eq!(truncate_module_name("items_for_display"), "items_display");
        assert_eq!(
            truncate_module_name("data_from_source_to_target"),
            "data_source_target"
        );
    }
}
