//! source_test_file — extracted from decompose.rs.

use super::DecomposePlan;
use crate::core::scaffold::load_extension_grammar;
use crate::extension::grammar_items;
use crate::extension::{self, ParsedItem};
use crate::Result;
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
        file: file.to_string(),
        strategy: strategy.to_string(),
        total_items: items.len(),
        groups,
        projected_audit_impact,
        checklist,
        warnings,
    })
}

pub(crate) fn project_audit_impact(groups: &[DecomposeGroup]) -> DecomposeAuditImpact {
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

pub(crate) fn source_to_test_file(target: &str) -> Option<String> {
    if !target.starts_with("src/") || !target.ends_with(".rs") {
        return None;
    }

    let without_src = target.strip_prefix("src/")?;
    let without_ext = without_src.strip_suffix(".rs")?;
    Some(format!("tests/{}_test.rs", without_ext))
}

pub(crate) fn parse_items(file: &str, content: &str) -> Option<Vec<ParsedItem>> {
    let ext = Path::new(file).extension()?.to_str()?;

    // Try core grammar engine first — faster and more robust than extension scripts
    if let Some(manifest) = extension::find_extension_for_file_ext(ext, "refactor") {
        if let Some(ext_path) = &manifest.extension_path {
            let grammar = load_extension_grammar(Path::new(ext_path), ext);
            if let Some(grammar) = grammar {
                let items = grammar_items::parse_items(content, &grammar);
                if !items.is_empty() {
                    return Some(items.into_iter().map(ParsedItem::from).collect());
                }
            }
        }

        // Fall back to extension script
        let command = serde_json::json!({
            "command": "parse_items",
            "file_path": file,
            "content": content,
        });
        let result = extension::run_refactor_script(&manifest, &command)?;
        return serde_json::from_value(result.get("items")?.clone()).ok();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_build_plan_default_path() {
        let file = "";
        let root = Path::new("");
        let strategy = "";
        let _result = build_plan(&file, &root, &strategy);
    }

    #[test]
    fn test_build_plan_has_expected_effects() {
        // Expected effects: file_read, mutation
        let file = "";
        let root = Path::new("");
        let strategy = "";
        let _ = build_plan(&file, &root, &strategy);
    }

    #[test]
    fn test_project_audit_impact_if_let_some_test_file_source_to_test_file_group_suggested_ta() {

        let _result = project_audit_impact();
    }

    #[test]
    fn test_project_audit_impact_has_expected_effects() {
        // Expected effects: mutation

        let _ = project_audit_impact();
    }

    #[test]
    fn test_source_to_test_file_target_starts_with_src_target_ends_with_rs() {

        let result = source_to_test_file();
        assert!(result.is_none(), "expected None for: !target.starts_with(\"src/\") || !target.ends_with(\".rs\")");
    }

    #[test]
    fn test_source_to_test_file_target_starts_with_src_target_ends_with_rs_2() {

        let _result = source_to_test_file();
    }

    #[test]
    fn test_source_to_test_file_target_starts_with_src_target_ends_with_rs_3() {

        let _result = source_to_test_file();
    }

    #[test]
    fn test_source_to_test_file_some_format_tests_test_rs_without_ext() {

        let result = source_to_test_file();
        assert!(result.is_some(), "expected Some for: Some(format!(\"tests/{{}}_test.rs\", without_ext))");
    }

    #[test]
    fn test_parse_items_default_path() {

        let _result = parse_items();
    }

    #[test]
    fn test_parse_items_if_let_some_manifest_extension_find_extension_for_file_ext_e() {

        let result = parse_items();
        assert!(result.is_some(), "expected Some for: if let Some(manifest) = extension::find_extension_for_file_ext(ext, \"refactor\") {{");
    }

    #[test]
    fn test_parse_items_let_some_manifest_extension_find_extension_for_file_ext_ext_() {

        let result = parse_items();
        assert!(result.is_some(), "expected Some for: let Some(manifest) = extension::find_extension_for_file_ext(ext, \"refactor\")");
    }

    #[test]
    fn test_parse_items_let_some_ext_path_manifest_extension_path() {

        let result = parse_items();
        assert!(result.is_some(), "expected Some for: let Some(ext_path) = &manifest.extension_path");
    }

    #[test]
    fn test_parse_items_items_is_empty() {

        let result = parse_items();
        assert!(result.is_some(), "expected Some for: !items.is_empty()");
    }

    #[test]
    fn test_parse_items_default_path_2() {

        let _result = parse_items();
    }

    #[test]
    fn test_parse_items_none() {

        let result = parse_items();
        assert!(result.is_none(), "expected None for: None");
    }

}
