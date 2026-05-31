use super::*;

#[test]
fn behavioral_test_names_not_flagged_as_orphaned() {
    // Regression: test_helpers_without_test_attr_not_counted_as_test_methods
    // was flagged as orphaned. The behavior-driven heuristic should skip
    // test names with 3+ segments where the first word doesn't match any
    // source method.
    let config = make_rust_config();
    let dir = std::env::temp_dir().join("homeboy_test_coverage_behavioral");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();

    let source = make_fp(
        "src/core/engine.rs",
        vec![
            "fingerprint_from_grammar",
            "extract_functions",
            "exact_hash",
            // Behavioral test names — these should NOT be flagged
            "test_helpers_without_test_attr_not_counted_as_test_methods",
            "test_replace_string_literals",
            "test_exact_hash_deterministic",
        ],
    );

    let findings = analyze_test_coverage(&dir, &[&source], &config);

    let orphaned: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
        })
        .collect();

    // test_replace_string_literals -> "replace_string_literals" — 3 segments,
    // not a direct prefix of any source method -> skip (behavioral)
    //
    // test_exact_hash_deterministic -> "exact_hash_deterministic" — 3 segments,
    // not a direct prefix of any source method -> skip (behavioral).
    // Even though "exact_hash" is a source method, "exact_hash_deterministic"
    // is a scenario description, not a method reference.
    //
    // test_helpers_without_test_attr_not_counted_as_test_methods -> 9 segments,
    // not a direct prefix of any source method -> skip (behavioral)

    let orphaned_names: Vec<String> = orphaned.iter().map(|f| f.description.clone()).collect();
    assert!(
        !orphaned_names.iter().any(|d| d.contains("helpers_without")),
        "Behavioral test name should NOT be flagged as orphaned. Orphaned: {:?}",
        orphaned_names
    );
    assert!(
        !orphaned_names.iter().any(|d| d.contains("replace_string")),
        "Behavioral test name should NOT be flagged as orphaned. Orphaned: {:?}",
        orphaned_names
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn short_rust_behavior_test_names_not_flagged_as_orphaned() {
    let config = make_rust_config();
    let dir = std::env::temp_dir().join("homeboy_test_coverage_short_behavioral");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/core/extension")).unwrap();

    let source = make_fp_split(
        "src/core/extension/version.rs",
        vec!["matches", "parse_php_import_path"],
        vec![
            "test_gt_matches",
            "test_lt_matches",
            "test_parse_php_import",
            "test_old_function",
        ],
    );

    let findings = analyze_test_coverage(&dir, &[&source], &config);
    let orphaned: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
        })
        .collect();

    assert_eq!(
        orphaned.len(),
        1,
        "short behavior names that cover live source methods should not be orphaned: {:?}",
        orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
    );
    assert!(orphaned[0].description.contains("old_function"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn common_behavior_scenario_names_not_flagged_as_orphaned() {
    let config = make_rust_config();
    let dir = std::env::temp_dir().join("homeboy_test_coverage_common_behavior_names");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/core")).unwrap();

    let source = make_fp_split(
        "src/core/update_check.rs",
        vec!["read_cache", "write_cache", "github_to_tracked"],
        vec![
            "test_cache_roundtrip",
            "test_display_roundtrip",
            "test_ignores_subscripts",
            "test_translates_open",
        ],
    );

    let findings = analyze_test_coverage(&dir, &[&source], &config);
    let orphaned: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
        })
        .collect();

    assert!(
        orphaned.is_empty(),
        "common behavior/scenario names should not be orphaned: {:?}",
        orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rust_include_wrapper_for_nested_test_path_is_not_misplaced() {
    let config = make_rust_config();
    let dir = std::env::temp_dir().join("homeboy_test_coverage_include_wrapper");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/core")).unwrap();
    std::fs::create_dir_all(dir.join("tests/core")).unwrap();

    let source = make_fp("src/core/deps.rs", vec!["status"]);
    let mut wrapper = make_fp("tests/deps_test.rs", vec!["status_reads_manifest"]);
    wrapper.content = "include!(\"core/deps_test.rs\");".to_string();

    let findings = analyze_test_coverage(&dir, &[&source, &wrapper], &config);
    let misplaced: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.kind == AuditFinding::OrphanedTest && f.description.contains("misplaced"))
        .collect();

    assert!(
        misplaced.is_empty(),
        "include wrappers keep nested test files discoverable: {:?}",
        misplaced.iter().map(|f| &f.description).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scenario_test_names_not_flagged_as_orphaned() {
    // Regression for #1120 / PR #1119: tests like "apply_replace_text" were
    // flagged as orphaned because the first word "apply" matched source
    // method "apply_edit_ops". These are scenario/behavioral tests for
    // apply_edit_ops_to_content(), not references to a deleted method.
    let config = make_rust_config();
    let dir = std::env::temp_dir().join("homeboy_test_coverage_scenario");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/core/engine")).unwrap();

    let source = make_fp(
        "src/core/engine/edit_op_apply.rs",
        vec![
            "resolve_anchor",
            "apply_edit_ops_to_content",
            "apply_edit_ops",
            "remove_from_reexport_block",
            // Scenario tests — none of these should be flagged
            "test_apply_replace_text",
            "test_apply_replace_text_not_found_errors",
            "test_apply_replace_text_line_out_of_range",
            "test_apply_remove_lines",
            "test_apply_insert_lines_at_line",
            "test_apply_insert_lines_after_imports",
            "test_apply_insert_lines_file_end",
            "test_apply_reexport_removal",
            "test_apply_multiple_ops_same_file",
            "test_apply_multiple_removals_bottom_to_top",
            "test_apply_combined_remove_and_insert",
            "test_resolve_anchor_at_line",
            "test_resolve_anchor_file_top",
            "test_resolve_anchor_after_imports_rust",
        ],
    );

    let findings = analyze_test_coverage(&dir, &[&source], &config);

    let orphaned: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
        })
        .collect();

    assert!(
        orphaned.is_empty(),
        "Scenario test names should NOT be flagged as orphaned. Flagged: {:?}",
        orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn production_method_with_test_prefix_not_flagged_orphaned() {
    // Regression for issue #1471: `ExtensionManifest::test_script()`
    // and `test_mapping()` are production accessors on a manifest struct —
    // public methods whose names happen to start with `test_`. They are
    // NOT `#[test]` functions. The detector used to flag them as orphaned
    // because `collect_test_methods_from_fp` filtered `.methods` by name
    // prefix, ignoring the structural `has_test_attr` signal. The
    // generator then auto-deleted them. Bug occurred three times in 26
    // hours (#1176 -> #1183 -> bench PR #1385 force-push reverts) until
    // this fix split test methods into their own `test_methods` vec.
    let config = make_rust_config();
    let dir = std::env::temp_dir().join("homeboy_test_coverage_prod_test_prefix");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/core/extension")).unwrap();

    // Model ExtensionManifest: production methods with `test_` names, no
    // inline tests. `test_methods` is empty (these are NOT #[test]).
    let source = make_fp_split(
        "src/core/extension/manifest.rs",
        vec![
            "lint_script",
            "build_script",
            "test_script",  // production accessor, looks like a test prefix
            "test_mapping", // production accessor, looks like a test prefix
            "autofix_verify",
        ],
        vec![], // no inline #[test] functions
    );

    let findings = analyze_test_coverage(&dir, &[&source], &config);

    let orphaned: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
        })
        .collect();

    assert!(
        orphaned.is_empty(),
        "Production methods named test_* must not be flagged as orphaned tests. \
         Flagged: {:?}",
        orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
    );

    // They should also not show up as missing-test findings for the
    // *nonexistent* source methods `script` / `mapping`.
    let missing_methods_referencing_stub: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            f.kind == AuditFinding::MissingTestMethod
                && (f.description.contains("'script'") || f.description.contains("'mapping'"))
        })
        .collect();
    assert!(
        missing_methods_referencing_stub.is_empty(),
        "test_script and test_mapping must not be interpreted as covering \
         source methods named 'script' / 'mapping'. Found: {:?}",
        missing_methods_referencing_stub
            .iter()
            .map(|f| &f.description)
            .collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// MissingTestMethod integration with substring matching (#1518).
// Unit tests for the `test_covers_method` predicate itself live in
// `super::idiomatic::tests`. These exercise the full `analyze_test_coverage`
// pipeline.

#[test]
fn missing_test_method_skipped_for_descriptive_test() {
    // Regression for #1518: a behavior-describing test name should be
    // recognized as coverage for the source method it references.
    let config = make_rust_config();
    let dir = std::env::temp_dir().join("homeboy_test_coverage_descriptive_inline");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/core")).unwrap();

    // Source method: fingerprint_content. Inline test:
    // fingerprint_content_matches_fingerprint_file (no `test_` prefix
    // because it's a behavior-describing name, but #[test]-attributed
    // upstream so it lives in `test_methods`).
    let source = make_fp_split(
        "src/core/fingerprint.rs",
        vec!["fingerprint_content"],
        vec!["fingerprint_content_matches_fingerprint_file"],
    );

    let findings = analyze_test_coverage(&dir, &[&source], &config);

    let missing: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.kind == AuditFinding::MissingTestMethod)
        .collect();
    assert!(
        missing.is_empty(),
        "Descriptive test name should be recognized as coverage. Findings: {:?}",
        missing.iter().map(|f| &f.description).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_test_method_still_emits_for_uncovered_method() {
    // Regression guard: substring matching must not turn into a free pass.
    // A source method with no test (literal or descriptive) still emits.
    let config = make_rust_config();
    let dir = std::env::temp_dir().join("homeboy_test_coverage_still_emits");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/core")).unwrap();

    let source = make_fp_split(
        "src/core/something.rs",
        vec!["something_uncovered"],
        vec!["totally_unrelated_test", "another_unrelated_one"],
    );

    let findings = analyze_test_coverage(&dir, &[&source], &config);

    let missing: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.kind == AuditFinding::MissingTestMethod)
        .collect();
    assert_eq!(
        missing.len(),
        1,
        "Uncovered source method must still emit. Findings: {:?}",
        missing.iter().map(|f| &f.description).collect::<Vec<_>>()
    );
    assert!(missing[0].description.contains("something_uncovered"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn orphaned_test_unaffected_by_substring_relaxation() {
    // Orphaned-test detection still uses the strict prefix path. A
    // `test_foo` with no `foo` source method emits an orphan finding,
    // unaffected by the new substring relaxation in coverage detection.
    let config = make_config();
    let dir = std::env::temp_dir().join("homeboy_test_coverage_orphan_strict");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();

    // Source has `bar`. Test file has `test_foo` (orphan, foo doesn't
    // exist) and `test_bar` (valid).
    let source = make_fp("src/mod.rs", vec!["bar"]);
    let test = make_fp("tests/mod_test.rs", vec!["test_foo", "test_bar"]);

    let findings = analyze_test_coverage(&dir, &[&source, &test], &config);

    let orphaned: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
        })
        .collect();
    assert_eq!(orphaned.len(), 1);
    assert!(orphaned[0].description.contains("test_foo"));

    let _ = std::fs::remove_dir_all(&dir);
}
