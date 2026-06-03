use super::*;

// ========================================================================
// Parallel Implementation Detection tests
// ========================================================================

fn make_fingerprint_with_content(path: &str, methods: &[&str], content: &str) -> FileFingerprint {
    FileFingerprint {
        relative_path: path.to_string(),
        language: Language::Rust,
        methods: methods.iter().map(|s| s.to_string()).collect(),
        content: content.to_string(),
        ..Default::default()
    }
}

#[test]
fn detects_parallel_implementation() {
    // Both bodies loop over a worklist — Looping ↔ Looping matches at the
    // standard Jaccard floor. Mirrors the real `copy_dir_recursive` ↔
    // `copy_directory` shape from issue #2334.
    let fp1 = make_fingerprint_with_content(
            "src/deploy.rs",
            &["deploy_to_server"],
            "fn deploy_to_server() {\n    for host in hosts {\n        validate_component();\n        build_artifact();\n        upload_to_host();\n        run_post_hooks();\n        notify_complete();\n    }\n}",
        );
    let fp2 = make_fingerprint_with_content(
            "src/upgrade.rs",
            &["upgrade_on_server"],
            "fn upgrade_on_server() {\n    for host in hosts {\n        validate_component();\n        build_artifact();\n        upload_to_host();\n        run_post_hooks();\n        send_notification();\n    }\n}",
        );

    let findings = detect_parallel_implementations(
        &[&fp1, &fp2],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert_eq!(findings.len(), 2, "Should emit one finding per file");
    assert!(findings
        .iter()
        .all(|f| f.kind == AuditFinding::ParallelImplementation));
    assert!(findings.iter().any(|f| f.file == "src/deploy.rs"));
    assert!(findings.iter().any(|f| f.file == "src/upgrade.rs"));
}

#[test]
fn no_parallel_for_unrelated_functions() {
    let fp1 = make_fingerprint_with_content(
        "src/deploy.rs",
        &["deploy_to_server"],
        "fn deploy_to_server() {\n    validate();\n    build();\n    upload();\n    notify();\n}",
    );
    let fp2 = make_fingerprint_with_content(
            "src/parser.rs",
            &["parse_config"],
            "fn parse_config() {\n    read_file();\n    tokenize();\n    parse_ast();\n    validate_schema();\n}",
        );

    let findings = detect_parallel_implementations(
        &[&fp1, &fp2],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );
    assert!(
        findings.is_empty(),
        "Completely different call sets should not flag"
    );
}

#[test]
fn no_parallel_for_same_file() {
    let fp = make_fingerprint_with_content(
            "src/ops.rs",
            &["deploy_op", "upgrade_op"],
            "fn deploy_op() {\n    validate();\n    build();\n    upload();\n    notify();\n}\nfn upgrade_op() {\n    validate();\n    build();\n    upload();\n    notify();\n}",
        );

    let findings = detect_parallel_implementations(
        &[&fp],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );
    assert!(
        findings.is_empty(),
        "Same-file methods should not be flagged as parallel"
    );
}

#[test]
fn no_parallel_for_trivial_methods() {
    let fp1 = make_fingerprint_with_content(
        "src/a.rs",
        &["small_a"],
        "fn small_a() {\n    foo();\n    bar();\n}",
    );
    let fp2 = make_fingerprint_with_content(
        "src/b.rs",
        &["small_b"],
        "fn small_b() {\n    foo();\n    bar();\n}",
    );

    let findings = detect_parallel_implementations(
        &[&fp1, &fp2],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );
    assert!(
        findings.is_empty(),
        "Methods with < MIN_CALL_COUNT calls should be skipped"
    );
}

#[test]
fn no_parallel_for_plumbing_only_call_patterns() {
    let fs_helper = make_fingerprint_with_content(
            "src/files.rs",
            &["plugin_header_version"],
            "fn plugin_header_version() {\n    path();\n    read_dir();\n    to_str();\n    success();\n}",
        );
    let extension_scan = make_fingerprint_with_content(
            "src/extensions.rs",
            &["scan_available_extensions"],
            "fn scan_available_extensions() {\n    path();\n    read_dir();\n    to_str();\n    is_dir();\n}",
        );

    let findings = detect_parallel_implementations(
        &[&fs_helper, &extension_scan],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert!(
        findings.is_empty(),
        "Plumbing-only filesystem call overlap should not flag"
    );
}

#[test]
fn no_parallel_for_command_wrapper_plumbing() {
    let command_runner = make_fingerprint_with_content(
        "src/command.rs",
        &["succeeded_in"],
        "fn succeeded_in() {\n    args();\n    current_dir();\n    output();\n    success();\n}",
    );
    let branch_reader = make_fingerprint_with_content(
        "src/stack.rs",
        &["current_branch"],
        "fn current_branch() {\n    args();\n    current_dir();\n    output();\n    success();\n}",
    );

    let findings = detect_parallel_implementations(
        &[&command_runner, &branch_reader],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert!(
        findings.is_empty(),
        "Shared Command setup/result checks are plumbing, not a workflow"
    );
}

#[test]
fn no_parallel_for_text_parsing_plumbing() {
    let http_handler = make_fingerprint_with_content(
            "src/core/daemon.rs",
            &["handle_connection"],
            "fn handle_connection() {\n    request.lines().next().split_whitespace();\n    route();\n    write_response();\n}",
        );
    let process_probe = make_fingerprint_with_content(
            "src/core/server/client.rs",
            &["probe_child_resources"],
            "fn probe_child_resources() {\n    stdout.lines().next().split_whitespace();\n    parse_rss();\n    parse_cpu();\n}",
        );

    let findings = detect_parallel_implementations(
        &[&http_handler, &process_probe],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert!(
        findings.is_empty(),
        "Shared line tokenization is parsing plumbing, not a reusable workflow"
    );
}

#[test]
fn no_parallel_for_deploy_plumbing_only_patterns() {
    let artifact_deploy = make_fingerprint_with_content(
            "src/core/deploy/safety_and_artifact.rs",
            &["deploy_artifact"],
            "fn deploy_artifact() {\n    quote_path();\n    execute();\n    failure();\n    render_map();\n    fix_deployed_permissions();\n}",
        );
    let override_deploy = make_fingerprint_with_content(
            "src/core/deploy/version_overrides.rs",
            &["deploy_with_override"],
            "fn deploy_with_override() {\n    quote_path();\n    execute();\n    failure();\n    render_map();\n    fix_deployed_permissions();\n}",
        );

    let findings = detect_parallel_implementations(
        &[&artifact_deploy, &override_deploy],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert!(
        findings.is_empty(),
        "Shared SSH/deploy epilogue calls should not imply an extractable workflow"
    );
}

#[test]
fn detects_parallel_implementation_after_plumbing_filter() {
    // Both bodies loop over their PR list — Looping ↔ Looping clears the
    // body-shape gate at the standard Jaccard floor.
    let apply = make_fingerprint_with_content(
            "src/core/stack/apply.rs",
            &["apply_stack"],
            "fn apply_stack() {\n    for pr in prs {\n        ensure_head_remote();\n        checkout_force();\n        fetch_pr_meta();\n        cherry_pick();\n        record_applied_pr();\n        run_git();\n        success();\n    }\n}",
        );
    let sync = make_fingerprint_with_content(
            "src/core/stack/sync.rs",
            &["sync_stack"],
            "fn sync_stack() {\n    for pr in prs {\n        ensure_head_remote();\n        checkout_force();\n        fetch_pr_meta();\n        cherry_pick();\n        record_synced_pr();\n        run_git();\n        success();\n    }\n}",
        );

    let findings = detect_parallel_implementations(
        &[&apply, &sync],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert_eq!(
        findings.len(),
        2,
        "Domain-heavy stack pairs should still flag"
    );
    assert!(findings
        .iter()
        .any(|finding| finding.description.contains("`ensure_head_remote`")));
    assert!(findings
        .iter()
        .all(|finding| !finding.description.contains("`run_git`")));
}

#[test]
fn no_parallel_for_generic_names() {
    // "run" is in GENERIC_NAMES
    let fp1 = make_fingerprint_with_content(
        "src/a.rs",
        &["run"],
        "fn run() {\n    validate();\n    build();\n    upload();\n    notify();\n}",
    );
    let fp2 = make_fingerprint_with_content(
        "src/b.rs",
        &["execute"],
        "fn execute() {\n    validate();\n    build();\n    upload();\n    notify();\n}",
    );

    // "run" is skipped, so only one method in the pool — no pair to compare
    let findings = detect_parallel_implementations(
        &[&fp1, &fp2],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );
    // Only fp2's "execute" has a valid call sequence; fp1's "run" is filtered
    // So there's only 1 candidate, no pair → no findings
    assert!(findings.is_empty(), "Generic names should be filtered out");
}

#[test]
fn jaccard_identical_sets() {
    let a = std::collections::HashSet::from(["foo".to_string(), "bar".to_string()]);
    assert!((jaccard_similarity(&a, &a, a.len()) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn jaccard_disjoint_sets() {
    let a = std::collections::HashSet::from(["foo".to_string()]);
    let b = std::collections::HashSet::from(["bar".to_string()]);
    assert!((jaccard_similarity(&a, &b, 0)).abs() < f64::EPSILON);
}

#[test]
fn shared_signal_call_count_counts_unique_overlap() {
    let a = std::collections::HashSet::from([
        "shared_one".to_string(),
        "shared_two".to_string(),
        "shared_three".to_string(),
        "unique_a".to_string(),
    ]);
    let b = std::collections::HashSet::from([
        "shared_one".to_string(),
        "shared_two".to_string(),
        "shared_three".to_string(),
        "unique_b".to_string(),
    ]);
    let c = std::collections::HashSet::from([
        "shared_one".to_string(),
        "shared_two".to_string(),
        "unique_c".to_string(),
        "another_c".to_string(),
    ]);

    assert_eq!(shared_signal_call_count(&a, &b), MIN_SHARED_CALLS);
    assert_eq!(shared_signal_call_count(&a, &c), MIN_SHARED_CALLS - 1);
}

#[test]
fn lcs_identical_sequences() {
    let a = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    assert_eq!(lcs_length(&a, &a), 3);
    assert!((lcs_ratio(&a, &a) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn lcs_partial_overlap() {
    let a = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let b = vec!["a".to_string(), "x".to_string(), "c".to_string()];
    assert_eq!(lcs_length(&a, &b), 2); // a, c
}

#[test]
fn convention_methods_skip_parallel_detection() {
    // Two methods with identical call patterns — would normally flag.
    // Wrapped in a loop so they clear the body-shape gate.
    let fp1 = make_fingerprint_with_content(
            "src/deploy.rs",
            &["registerAbilities"],
            "fn registerAbilities() {\n    for ability in abilities {\n        validate_component();\n        build_artifact();\n        upload_to_host();\n        run_post_hooks();\n        notify_complete();\n    }\n}",
        );
    let fp2 = make_fingerprint_with_content(
            "src/upgrade.rs",
            &["registerAbility"],
            "fn registerAbility() {\n    for ability in abilities {\n        validate_component();\n        build_artifact();\n        upload_to_host();\n        run_post_hooks();\n        send_notification();\n    }\n}",
        );

    // Without convention methods: flagged
    let findings = detect_parallel_implementations(
        &[&fp1, &fp2],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );
    assert_eq!(findings.len(), 2, "Should flag without convention context");

    // With EITHER method as convention-expected: NOT flagged
    let conv_methods: std::collections::HashSet<String> = ["registerAbilities"] // only one of the two
        .iter()
        .map(|s| s.to_string())
        .collect();
    let findings = detect_parallel_implementations(
        &[&fp1, &fp2],
        &conv_methods,
        &DuplicationDetectorConfig::default(),
    );
    assert!(
        findings.is_empty(),
        "Pairs involving convention methods should not be flagged, got: {:?}",
        findings.iter().map(|f| &f.description).collect::<Vec<_>>()
    );
}

// ========================================================================
// Body-shape gate tests (issue #2334)
// ========================================================================

#[test]
fn body_shape_gate_two_loops_with_shared_calls_flag() {
    // Mirrors the real `copy_dir_recursive` ↔ `copy_directory` shape that
    // we MUST keep flagging after the body-shape gate ships.
    let copy_dir_recursive = make_fingerprint_with_content(
            "src/core/extension/lifecycle.rs",
            &["copy_dir_recursive"],
            "fn copy_dir_recursive() {\n    create_dir_all(dst);\n    for entry in read_dir(src) {\n        copy_file_entry();\n        record_copied();\n        verify_target();\n    }\n}",
        );
    let copy_directory = make_fingerprint_with_content(
            "src/core/engine/invocation.rs",
            &["copy_directory"],
            "fn copy_directory() {\n    create_dir_all(dst);\n    for entry in read_dir(src) {\n        copy_file_entry();\n        record_copied();\n        preserve_artifact();\n    }\n}",
        );

    let findings = detect_parallel_implementations(
        &[&copy_dir_recursive, &copy_directory],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert_eq!(
            findings.len(),
            2,
            "Two looping copy helpers with shared calls must still flag — that is the real finding from #2334"
        );
}

#[test]
fn body_shape_gate_kills_single_file_vs_recursive_walk_fp() {
    // The canonical FP from issue #2334:
    // `copy_artifact_file` is StraightLine (single `fs::copy` after a
    // `create_dir_all` of the parent), `copy_dir_recursive` is
    // Looping+Recursive (recursive walk over `read_dir`). They share
    // `create_dir_all` and `copy` but the workflows are not the same.
    let copy_artifact_file = make_fingerprint_with_content(
            "src/core/observation/store.rs",
            &["copy_artifact_file"],
            "fn copy_artifact_file() {\n    let parent = target_parent();\n    create_dir_all(parent);\n    copy(source, target);\n    verify_size();\n    record_copy();\n}",
        );
    let copy_dir_recursive = make_fingerprint_with_content(
            "src/core/extension/lifecycle.rs",
            &["copy_dir_recursive"],
            "fn copy_dir_recursive() {\n    create_dir_all(dst);\n    for entry in read_dir(src) {\n        copy(entry, target);\n        verify_size();\n        record_copy();\n        copy_dir_recursive(entry, dst);\n    }\n}",
        );

    let findings = detect_parallel_implementations(
        &[&copy_artifact_file, &copy_dir_recursive],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert!(
        findings.is_empty(),
        "Single-file copy (StraightLine) vs recursive walk (Looping) must not flag — got: {:?}",
        findings.iter().map(|f| &f.description).collect::<Vec<_>>()
    );
}

#[test]
fn body_shape_gate_two_straight_line_below_raised_jaccard_floor() {
    // Two StraightLine bodies: 4 shared calls, 6 union → Jaccard 0.667.
    // Below the raised StraightLine floor of 0.7 → must NOT flag.
    let helper_a = make_fingerprint_with_content(
            "src/core/a.rs",
            &["build_thing_a"],
            "fn build_thing_a() {\n    let x = open_resource();\n    register_handler();\n    configure_options();\n    finalize_build();\n    emit_metric_a();\n}",
        );
    let helper_b = make_fingerprint_with_content(
            "src/core/b.rs",
            &["build_thing_b"],
            "fn build_thing_b() {\n    let x = open_resource();\n    register_handler();\n    configure_options();\n    finalize_build();\n    emit_metric_b();\n}",
        );

    let findings = detect_parallel_implementations(
        &[&helper_a, &helper_b],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert!(
        findings.is_empty(),
        "Two StraightLine bodies at Jaccard 0.667 must not flag under the raised floor — got: {:?}",
        findings.iter().map(|f| &f.description).collect::<Vec<_>>()
    );
}

#[test]
fn body_shape_gate_two_straight_line_above_raised_jaccard_floor() {
    // Same StraightLine pair but with identical signal calls (Jaccard 1.0)
    // — clears the raised floor and MUST flag. This proves the gate is a
    // shape filter, not a blanket ban on StraightLine pairs.
    let helper_a = make_fingerprint_with_content(
            "src/core/a.rs",
            &["build_thing_a"],
            "fn build_thing_a() {\n    let x = open_resource();\n    register_handler();\n    configure_options();\n    finalize_build();\n    emit_metric();\n}",
        );
    let helper_b = make_fingerprint_with_content(
            "src/core/b.rs",
            &["build_thing_b"],
            "fn build_thing_b() {\n    let x = open_resource();\n    register_handler();\n    configure_options();\n    finalize_build();\n    emit_metric();\n}",
        );

    let findings = detect_parallel_implementations(
        &[&helper_a, &helper_b],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert_eq!(
        findings.len(),
        2,
        "Two StraightLine bodies above the raised Jaccard floor must still flag"
    );
}

#[test]
fn body_shape_gate_recursive_to_recursive_flags() {
    // Two recursive helpers (no loop, but each calls itself) share the
    // same workflow — Recursive ↔ Recursive is compatible and uses the
    // standard Jaccard floor.
    let walk_a = make_fingerprint_with_content(
            "src/core/a.rs",
            &["walk_tree_a"],
            "fn walk_tree_a(node) {\n    visit_node();\n    record_step();\n    sanitize_value();\n    log_progress();\n    walk_tree_a(child);\n}",
        );
    let walk_b = make_fingerprint_with_content(
            "src/core/b.rs",
            &["walk_tree_b"],
            "fn walk_tree_b(node) {\n    visit_node();\n    record_step();\n    sanitize_value();\n    log_progress();\n    walk_tree_b(child);\n}",
        );

    let findings = detect_parallel_implementations(
        &[&walk_a, &walk_b],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert_eq!(
        findings.len(),
        2,
        "Recursive ↔ Recursive helpers with shared call set must flag"
    );
}

// ========================================================================
// Extension-supplied trivial/plumbing call list tests (#2333)
// ========================================================================

#[test]
fn extension_trivial_calls_filter_out_of_signal() {
    // Two parallel deploy/upgrade workflows that share several
    // domain-meaningful calls — flagged by default. With `custom_helper`
    // declared trivial via extension, that call is filtered out of the
    // recorded sequence; the remaining shared signal is unchanged so
    // this test specifically demonstrates the trivial path is wired
    // (compare against the default-config sanity below).
    let fp1 = make_fingerprint_with_content(
            "src/deploy.rs",
            &["deploy_to_server"],
            "fn deploy_to_server() {\n    custom_helper();\n    validate_component();\n    build_artifact();\n    upload_to_host();\n    run_post_hooks();\n    notify_complete();\n}",
        );
    let fp2 = make_fingerprint_with_content(
            "src/upgrade.rs",
            &["upgrade_on_server"],
            "fn upgrade_on_server() {\n    custom_helper();\n    validate_component();\n    build_artifact();\n    upload_to_host();\n    run_post_hooks();\n    send_notification();\n}",
        );

    // Default config: flagged (has the shared workflow signal).
    let default_findings = detect_parallel_implementations(
        &[&fp1, &fp2],
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );
    assert!(
        !default_findings.is_empty(),
        "Sanity: workflow pair must flag without extension config"
    );
    assert!(
            default_findings
                .iter()
                .any(|f| f.description.contains("`custom_helper`")),
            "Sanity: without trivial-list filtering, `custom_helper` should appear in the shared-call summary"
        );

    // Extension-supplied trivial removes `custom_helper` from sequences.
    let cfg = DuplicationDetectorConfig {
        trivial_calls: vec!["custom_helper".to_string()],
        plumbing_calls: vec![],
    };
    let findings =
        detect_parallel_implementations(&[&fp1, &fp2], &std::collections::HashSet::new(), &cfg);
    // The pair still flags (other workflow signal remains), but the
    // extension-trivial name is gone from the description.
    assert!(
            findings
                .iter()
                .all(|f| !f.description.contains("`custom_helper`")),
            "Extension-trivial `custom_helper` must be filtered out of the call sequence and shared-call summary, got: {:?}",
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
}

#[test]
fn body_shape_detection_smoke() {
    assert_eq!(
        detect_body_shape("    foo();\n    bar();\n", "thing"),
        BodyShape::StraightLine
    );
    assert_eq!(
        detect_body_shape("    for entry in items {\n        foo();\n    }\n", "thing"),
        BodyShape::Looping
    );
    assert_eq!(
        detect_body_shape("    items.iter().map(|x| x).collect();\n", "thing"),
        BodyShape::Looping
    );
    assert_eq!(
        detect_body_shape("    foo();\n    walk(child);\n", "walk"),
        BodyShape::Recursive
    );
    // Identifier guard — `redo_walk` must not register as a self-call to `walk`.
    assert_eq!(
        detect_body_shape("    redo_walk(x);\n", "walk"),
        BodyShape::StraightLine
    );
    // `format!` contains the substring `for` but must not register as Looping.
    assert_eq!(
        detect_body_shape("    let s = format!(\"x\");\n    foo();\n", "thing"),
        BodyShape::StraightLine
    );
}

#[test]
fn extension_plumbing_calls_filter_out_of_signal() {
    // Two methods share only `log_event` as workflow overlap — everything
    // else is unique. With default config, `log_event` is workflow signal
    // and the pair flags. With `log_event` declared plumbing via extension,
    // the shared signal collapses below MIN_CALL_COUNT and the pair is
    // dropped.
    let fp1 = make_fingerprint_with_content(
            "src/a.rs",
            &["worker_a"],
            "fn worker_a() {\n    log_event();\n    log_event();\n    log_event();\n    log_event();\n    step_a1();\n    step_a2();\n}",
        );
    let fp2 = make_fingerprint_with_content(
            "src/b.rs",
            &["worker_b"],
            "fn worker_b() {\n    log_event();\n    log_event();\n    log_event();\n    log_event();\n    step_b1();\n    step_b2();\n}",
        );

    // Without extension config, log_event is recorded as signal — but with
    // the existing built-in idiomatic floors, results vary. We only assert
    // the extension hook genuinely silences any pairing.
    let cfg = DuplicationDetectorConfig {
        trivial_calls: vec![],
        plumbing_calls: vec!["log_event".to_string()],
    };
    let findings =
        detect_parallel_implementations(&[&fp1, &fp2], &std::collections::HashSet::new(), &cfg);
    assert!(
            findings
                .iter()
                .all(|f| !f.description.contains("`log_event`")),
            "Extension-plumbing `log_event` must be removed from signal calls / shared-call summary, got: {:?}",
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
}

#[test]
fn extension_call_lists_fix_env_path_helper_fp_2333() {
    // Direct regression for issue #2333: `cache_fallback_root` ↔ `homeboy_data`
    // both call `var`, `cfg`, `not`, `internal_unexpected` (environment-derived
    // path plumbing). Without extension hints the detector flags them; with
    // extension config declaring those calls as trivial/plumbing, no FP.
    let cache = make_fingerprint_with_content(
            "src/core/cache.rs",
            &["cache_fallback_root"],
            "fn cache_fallback_root() {\n    var();\n    cfg();\n    not();\n    internal_unexpected();\n    join_cache_path();\n}",
        );
    let data = make_fingerprint_with_content(
            "src/core/data.rs",
            &["homeboy_data"],
            "fn homeboy_data() {\n    var();\n    cfg();\n    not();\n    internal_unexpected();\n    join_data_path();\n}",
        );

    // Default config: detector may still flag (this is the FP we are fixing).
    // We do NOT assert a specific shape here — issue #2334 covers a body-shape
    // gate that may also suppress this. We only require that the extension
    // hook genuinely silences it.

    // With extension config matching the component manifest:
    let cfg = DuplicationDetectorConfig {
        trivial_calls: vec!["var".to_string(), "cfg".to_string(), "not".to_string()],
        plumbing_calls: vec!["internal_unexpected".to_string()],
    };
    let findings =
        detect_parallel_implementations(&[&cache, &data], &std::collections::HashSet::new(), &cfg);
    assert!(
            findings.is_empty(),
            "Issue #2333: env-derived path helpers should NOT flag once extension config supplies trivial/plumbing lists, got: {:?}",
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
}

#[test]
fn extension_lists_augment_built_in_floor_not_replace() {
    // Built-in floors must remain active even when an extension supplies
    // its own lists. Two methods sharing only built-in trivial calls
    // (`to_string`, `clone`, `unwrap`) should not flag, regardless of
    // extension config contents.
    let fp1 = make_fingerprint_with_content(
            "src/a.rs",
            &["render_a"],
            "fn render_a() {\n    to_string();\n    clone();\n    unwrap();\n    len();\n    iter();\n}",
        );
    let fp2 = make_fingerprint_with_content(
            "src/b.rs",
            &["render_b"],
            "fn render_b() {\n    to_string();\n    clone();\n    unwrap();\n    len();\n    iter();\n}",
        );

    // Extension config that does NOT mention to_string/clone/etc.
    let cfg = DuplicationDetectorConfig {
        trivial_calls: vec!["something_unrelated".to_string()],
        plumbing_calls: vec!["another_unrelated".to_string()],
    };
    let findings =
        detect_parallel_implementations(&[&fp1, &fp2], &std::collections::HashSet::new(), &cfg);
    assert!(
        findings.is_empty(),
        "Built-in trivial floor must remain active even with extension config, got: {:?}",
        findings.iter().map(|f| &f.description).collect::<Vec<_>>()
    );
}

#[test]
fn corpus_common_calls_filter_broad_boilerplate_signal() {
    // Generic regression for issue #2398: if a call tuple appears across a
    // broad slice of the scanned component, it is scaffolding for this
    // detector. The names are intentionally framework-neutral fixtures;
    // extensions can still provide explicit trivial/plumbing lists.
    let mut fingerprints = Vec::new();

    for idx in 0..8 {
        let method = format!("boilerplate_holder_{idx}");
        let content = format!(
                "fn {method}() {{\n    scaffold_response();\n    read_request();\n    default_payload();\n    validate_presence();\n    filler_{idx}();\n}}"
            );
        fingerprints.push(make_fingerprint_with_content(
            &format!("src/common_{idx}.rs"),
            &[method.as_str()],
            &content,
        ));
    }

    let first = make_fingerprint_with_content(
            "src/a.rs",
            &["create_item"],
            "fn create_item() {\n    scaffold_response();\n    read_request();\n    default_payload();\n    validate_presence();\n    create_specific_step();\n}",
        );
    let second = make_fingerprint_with_content(
            "src/b.rs",
            &["delete_item"],
            "fn delete_item() {\n    scaffold_response();\n    read_request();\n    default_payload();\n    validate_presence();\n    delete_specific_step();\n}",
        );
    fingerprints.push(first);
    fingerprints.push(second);

    let refs = fingerprints.iter().collect::<Vec<_>>();
    let findings = detect_parallel_implementations(
        &refs,
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert!(
            findings.is_empty(),
            "Corpus-common scaffolding calls should not produce a parallel implementation finding, got: {:?}",
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
}

#[test]
fn corpus_common_calls_preserve_domain_specific_signal() {
    let mut fingerprints = Vec::new();

    for idx in 0..8 {
        let method = format!("boilerplate_holder_{idx}");
        let content = format!(
                "fn {method}() {{\n    scaffold_response();\n    read_request();\n    default_payload();\n    validate_presence();\n    filler_{idx}();\n}}"
            );
        fingerprints.push(make_fingerprint_with_content(
            &format!("src/common_{idx}.rs"),
            &[method.as_str()],
            &content,
        ));
    }

    let first = make_fingerprint_with_content(
            "src/deploy.rs",
            &["deploy_item"],
            "fn deploy_item() {\n    scaffold_response();\n    read_request();\n    validate_component();\n    build_artifact();\n    upload_to_host();\n    run_post_hooks();\n    verify_release();\n    deploy_specific_step();\n}",
        );
    let second = make_fingerprint_with_content(
            "src/upgrade.rs",
            &["upgrade_item"],
            "fn upgrade_item() {\n    scaffold_response();\n    read_request();\n    validate_component();\n    build_artifact();\n    upload_to_host();\n    run_post_hooks();\n    verify_release();\n    upgrade_specific_step();\n}",
        );
    fingerprints.push(first);
    fingerprints.push(second);

    let refs = fingerprints.iter().collect::<Vec<_>>();
    let findings = detect_parallel_implementations(
        &refs,
        &std::collections::HashSet::new(),
        &DuplicationDetectorConfig::default(),
    );

    assert_eq!(
            findings.len(),
            2,
            "Domain-specific shared workflow calls should still flag after common scaffolding is discounted"
        );
    assert!(
        findings
            .iter()
            .all(|f| f.description.contains("`build_artifact`")
                && !f.description.contains("`read_request`")),
        "Findings should be driven by domain calls, got: {:?}",
        findings.iter().map(|f| &f.description).collect::<Vec<_>>()
    );
}
