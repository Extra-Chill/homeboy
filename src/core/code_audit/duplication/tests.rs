use super::*;
use crate::core::code_audit::conventions::Language;

fn make_fingerprint(path: &str, methods: &[&str], hashes: &[(&str, &str)]) -> FileFingerprint {
    make_fingerprint_with_structural(path, methods, hashes, &[])
}

fn make_fingerprint_with_structural(
    path: &str,
    methods: &[&str],
    hashes: &[(&str, &str)],
    structural: &[(&str, &str)],
) -> FileFingerprint {
    FileFingerprint {
        relative_path: path.to_string(),
        language: Language::Rust,
        methods: methods.iter().map(|s| s.to_string()).collect(),
        method_hashes: hashes
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        structural_hashes: structural
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..Default::default()
    }
}

#[test]
fn detects_exact_duplicate() {
    let fp1 = make_fingerprint("src/utils/io.rs", &["is_zero"], &[("is_zero", "abc123")]);
    let fp2 = make_fingerprint(
        "src/utils/validation.rs",
        &["is_zero"],
        &[("is_zero", "abc123")],
    );

    let findings = detect_duplicates(&[&fp1, &fp2], &std::collections::HashSet::new());

    assert_eq!(findings.len(), 2, "Should emit one finding per location");
    assert!(findings
        .iter()
        .all(|f| f.kind == AuditFinding::DuplicateFunction));
    assert!(findings.iter().any(|f| f.file == "src/utils/io.rs"));
    assert!(findings.iter().any(|f| f.file == "src/utils/validation.rs"));
    assert!(findings[0].description.contains("is_zero"));
}

#[test]
fn duplicate_functions_under_tests_are_info_findings() {
    let fp1 = make_fingerprint(
        "tests/import/ability-smoke.php",
        &["imp_assert"],
        &[("imp_assert", "abc123")],
    );
    let fp2 = make_fingerprint(
        "tests/import/adapter-smoke.php",
        &["imp_assert"],
        &[("imp_assert", "abc123")],
    );

    let findings = detect_duplicates(&[&fp1, &fp2], &std::collections::HashSet::new());

    assert_eq!(findings.len(), 2);
    assert!(findings
        .iter()
        .all(|finding| finding.severity == Severity::Info));
    assert!(findings
        .iter()
        .all(|finding| finding.suggestion.contains("shared test helper")));
}

#[test]
fn no_duplicates_different_hashes() {
    let fp1 = make_fingerprint("src/a.rs", &["process"], &[("process", "hash_a")]);
    let fp2 = make_fingerprint("src/b.rs", &["process"], &[("process", "hash_b")]);

    let findings = detect_duplicates(&[&fp1, &fp2], &std::collections::HashSet::new());
    assert!(
        findings.is_empty(),
        "Different hashes should not flag duplicates"
    );
}

#[test]
fn no_duplicates_single_location() {
    let fp = make_fingerprint("src/only.rs", &["unique_fn"], &[("unique_fn", "abc123")]);

    let findings = detect_duplicates(&[&fp], &std::collections::HashSet::new());
    assert!(findings.is_empty(), "Single location is not a duplicate");
}

#[test]
fn three_way_duplicate() {
    let fp1 = make_fingerprint("src/a.rs", &["helper"], &[("helper", "same_hash")]);
    let fp2 = make_fingerprint("src/b.rs", &["helper"], &[("helper", "same_hash")]);
    let fp3 = make_fingerprint("src/c.rs", &["helper"], &[("helper", "same_hash")]);

    let findings = detect_duplicates(&[&fp1, &fp2, &fp3], &std::collections::HashSet::new());

    assert_eq!(findings.len(), 3, "Should flag all 3 locations");
    assert!(findings[0].suggestion.contains("3 files"));
}

#[test]
fn empty_method_hashes_no_findings() {
    let fp1 = make_fingerprint("src/a.rs", &["foo", "bar"], &[]);
    let fp2 = make_fingerprint("src/b.rs", &["foo", "bar"], &[]);

    let findings = detect_duplicates(&[&fp1, &fp2], &std::collections::HashSet::new());
    assert!(
        findings.is_empty(),
        "No hashes means no duplication findings"
    );
}

#[test]
fn mixed_duplicates_and_unique() {
    let fp1 = make_fingerprint(
        "src/a.rs",
        &["shared", "unique_a"],
        &[("shared", "same"), ("unique_a", "hash_a")],
    );
    let fp2 = make_fingerprint(
        "src/b.rs",
        &["shared", "unique_b"],
        &[("shared", "same"), ("unique_b", "hash_b")],
    );

    let findings = detect_duplicates(&[&fp1, &fp2], &std::collections::HashSet::new());

    assert_eq!(findings.len(), 2, "Only 'shared' should be flagged");
    assert!(findings.iter().all(|f| f.description.contains("shared")));
}

// ========================================================================
// DuplicateGroup / canonical selection tests
// ========================================================================

#[test]
fn group_picks_canonical_by_shortest_path() {
    let fp1 = make_fingerprint("src/core/deep/nested/helper.rs", &["foo"], &[("foo", "h1")]);
    let fp2 = make_fingerprint("src/utils.rs", &["foo"], &[("foo", "h1")]);

    let groups = detect_duplicate_groups(&[&fp1, &fp2]);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].canonical_file, "src/utils.rs");
    assert_eq!(
        groups[0].remove_from,
        vec!["src/core/deep/nested/helper.rs"]
    );
}

#[test]
fn group_prefers_utils_directory() {
    let fp1 = make_fingerprint("src/core/a.rs", &["shared"], &[("shared", "h1")]);
    let fp2 = make_fingerprint("src/utils/helpers.rs", &["shared"], &[("shared", "h1")]);

    let groups = detect_duplicate_groups(&[&fp1, &fp2]);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].canonical_file, "src/utils/helpers.rs");
    assert_eq!(groups[0].remove_from, vec!["src/core/a.rs"]);
}

#[test]
fn group_alphabetical_tiebreaker() {
    let fp1 = make_fingerprint("src/b.rs", &["dup"], &[("dup", "h1")]);
    let fp2 = make_fingerprint("src/a.rs", &["dup"], &[("dup", "h1")]);

    let groups = detect_duplicate_groups(&[&fp1, &fp2]);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].canonical_file, "src/a.rs");
}

#[test]
fn group_three_way_has_two_removals() {
    let fp1 = make_fingerprint("src/a.rs", &["f"], &[("f", "h")]);
    let fp2 = make_fingerprint("src/b.rs", &["f"], &[("f", "h")]);
    let fp3 = make_fingerprint("src/c.rs", &["f"], &[("f", "h")]);

    let groups = detect_duplicate_groups(&[&fp1, &fp2, &fp3]);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].remove_from.len(), 2);
    assert!(!groups[0].remove_from.contains(&groups[0].canonical_file));
}

// ========================================================================
// Near-duplicate detection tests
// ========================================================================

mod near_duplicates {
    use super::*;

    /// Helper to build a fingerprint with content for body-line counting.
    fn make_fp_with_content(
        path: &str,
        content: &str,
        hashes: &[(&str, &str)],
        structural: &[(&str, &str)],
    ) -> FileFingerprint {
        let mut fp = make_fingerprint_with_structural(path, &[], hashes, structural);
        fp.content = content.to_string();
        fp
    }

    #[test]
    fn near_duplicate_detected_when_structural_match_but_exact_differs() {
        // cache_path in two files: same structure, different constants.
        // Use a 3-body-line shape so the function clears MIN_BODY_LINES
        // (the trivial-body filter); the structural twins differ only by
        // the constant referenced.
        let content_a = "fn cache_path() -> Option<PathBuf> {\n    let base = paths::homeboy().ok()?;\n    let file = base.join(CACHE_A);\n    Some(file)\n}\n";
        let content_b = "fn cache_path() -> Option<PathBuf> {\n    let base = paths::homeboy().ok()?;\n    let file = base.join(CACHE_B);\n    Some(file)\n}\n";

        let fp1 = make_fp_with_content(
            "src/core/update_check.rs",
            content_a,
            &[("cache_path", "hash_a")],
            &[("cache_path", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/ext_update_check.rs",
            content_b,
            &[("cache_path", "hash_b")],
            &[("cache_path", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);

        assert_eq!(findings.len(), 2, "Should flag both locations");
        assert!(findings
            .iter()
            .all(|f| f.kind == AuditFinding::NearDuplicate));
        assert!(findings[0].description.contains("cache_path"));
        assert_eq!(findings[0].severity, Severity::Info);
    }

    #[test]
    fn near_duplicate_skips_exact_duplicates() {
        // If exact hashes match, exact-duplicate detector already handles it
        let fp1 = make_fingerprint_with_structural(
            "src/a.rs",
            &["helper"],
            &[("helper", "SAME")],
            &[("helper", "SAME_STRUCT")],
        );
        let fp2 = make_fingerprint_with_structural(
            "src/b.rs",
            &["helper"],
            &[("helper", "SAME")],
            &[("helper", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "Exact duplicates should be excluded");
    }

    #[test]
    fn near_duplicate_skips_generic_names() {
        let content = "fn run() {\n    do_something();\n    do_more();\n}\n";
        let fp1 = make_fp_with_content(
            "src/core/a.rs",
            content,
            &[("run", "hash_a")],
            &[("run", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/b.rs",
            content,
            &[("run", "hash_b")],
            &[("run", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(
            findings.is_empty(),
            "'run' is a generic name — should be skipped"
        );
    }

    #[test]
    fn near_duplicate_skips_command_core_pairs() {
        let content = "fn deploy_site() {\n    connect();\n    upload();\n    verify();\n}\n";
        let fp1 = make_fp_with_content(
            "src/commands/deploy.rs",
            content,
            &[("deploy_site", "hash_a")],
            &[("deploy_site", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/deploy.rs",
            content,
            &[("deploy_site", "hash_b")],
            &[("deploy_site", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "Command↔core pair should be skipped");
    }

    #[test]
    fn near_duplicate_skips_trivial_functions() {
        // default_true is only 1 line — too trivial to refactor
        let content = "fn default_true() -> bool { true }\n";
        let fp1 = make_fp_with_content(
            "src/core/defaults.rs",
            content,
            &[("default_true", "hash_a")],
            &[("default_true", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/project.rs",
            content,
            &[("default_true", "hash_b")],
            &[("default_true", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "Trivial functions should be skipped");
    }

    #[test]
    fn near_duplicate_not_skipped_for_multi_line_core_functions() {
        // Non-trivial functions in core/ (not commands/) SHOULD be flagged
        let content = "fn cache_path() -> Option<PathBuf> {\n    let base = paths::homeboy()?;\n    let file = base.join(FILENAME);\n    Some(file)\n}\n";
        let fp1 = make_fp_with_content(
            "src/core/update.rs",
            content,
            &[("cache_path", "hash_a")],
            &[("cache_path", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/ext_update.rs",
            content,
            &[("cache_path", "hash_b")],
            &[("cache_path", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert_eq!(
            findings.len(),
            2,
            "Non-trivial core↔core near-duplicates should be flagged"
        );
    }

    #[test]
    fn near_duplicate_skips_all_command_files() {
        // Multiple command files with same structural hash — normal pattern
        let content = "fn components() {\n    let list = config::list();\n    for item in list {\n        output::print(item);\n    }\n}\n";
        let fp1 = make_fp_with_content(
            "src/commands/fleet.rs",
            content,
            &[("components", "hash_a")],
            &[("components", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/commands/project.rs",
            content,
            &[("components", "hash_b")],
            &[("components", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "All-commands group should be skipped");
    }

    // ========================================================================
    // count_body_lines — measures body lines strictly between braces (#1517)
    // ========================================================================

    #[test]
    fn count_body_lines_zero_for_single_line_body() {
        // `fn x() -> u32 { 0 }` — opening and closing brace on the same line.
        // Zero lines strictly between them, so zero body lines.
        let content = "fn x() -> u32 { 0 }\n";
        let mut fp = make_fingerprint("src/x.rs", &["x"], &[]);
        fp.content = content.to_string();

        assert_eq!(
            count_body_lines(&fp, "x"),
            0,
            "single-line body should report 0 body lines"
        );
    }

    #[test]
    fn count_body_lines_one_for_three_line_shape() {
        // The standard formatter shape:
        //   fn x() -> u32 {
        //       0
        //   }
        // Exactly one line strictly between the braces.
        let content = "fn x() -> u32 {\n    0\n}\n";
        let mut fp = make_fingerprint("src/x.rs", &["x"], &[]);
        fp.content = content.to_string();

        assert_eq!(
            count_body_lines(&fp, "x"),
            1,
            "three-line shape should report 1 body line"
        );
    }

    #[test]
    fn count_body_lines_counts_actual_body_statements() {
        // Multi-line body with 4 statements between the braces.
        let content = "fn process(items: &[Item]) -> Result {\n    let mut out = Vec::new();\n    for item in items {\n        out.push(item.clone());\n    }\n    Ok(out)\n}\n";
        let mut fp = make_fingerprint("src/process.rs", &["process"], &[]);
        fp.content = content.to_string();

        // Lines strictly between `{` and `}`:
        //   let mut out = Vec::new();
        //   for item in items {
        //       out.push(item.clone());
        //   }
        //   Ok(out)
        // → 5 body lines.
        assert_eq!(
            count_body_lines(&fp, "process"),
            5,
            "should count actual body lines (5), not the wrapping span (7)"
        );
    }

    #[test]
    fn near_duplicate_skips_idiomatic_collection_methods() {
        // The triggering case for #1517: every Vec/HashMap wrapper in the
        // ecosystem defines `fn len(&self) -> usize { self.inner.len() }`,
        // and Clippy's `len_without_is_empty` lint requires `is_empty`
        // alongside it. Two structs each defining both methods must NOT
        // produce near_duplicate findings.
        let content_a = "struct A { inner: Vec<u8> }\nimpl A {\n    pub fn len(&self) -> usize {\n        self.inner.len()\n    }\n    pub fn is_empty(&self) -> bool {\n        self.inner.is_empty()\n    }\n}\n";
        let content_b = "struct B { inner: HashMap<String, u32> }\nimpl B {\n    pub fn len(&self) -> usize {\n        self.inner.len()\n    }\n    pub fn is_empty(&self) -> bool {\n        self.inner.is_empty()\n    }\n}\n";

        let fp1 = make_fp_with_content(
            "src/core/a.rs",
            content_a,
            &[("len", "hash_a_len"), ("is_empty", "hash_a_emp")],
            &[("len", "SAME_LEN"), ("is_empty", "SAME_EMP")],
        );
        let fp2 = make_fp_with_content(
            "src/core/b.rs",
            content_b,
            &[("len", "hash_b_len"), ("is_empty", "hash_b_emp")],
            &[("len", "SAME_LEN"), ("is_empty", "SAME_EMP")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(
            findings.is_empty(),
            "idiomatic collection-wrapper methods (`len`, `is_empty`) must not be flagged as near-duplicates; got {} finding(s): {:?}",
            findings.len(),
            findings.iter().map(|f| &f.description).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn near_duplicate_still_flags_real_duplicates() {
        // Regression guard against over-suppressing: a non-trivially-named
        // method with identical structural hash but different body hashes
        // across two files (and a 3+ body-line shape) MUST still be flagged.
        let content_a = "fn compute_fixability(item: &Item) -> bool {\n    let score = item.score();\n    let threshold = THRESHOLD_A;\n    score > threshold\n}\n";
        let content_b = "fn compute_fixability(item: &Item) -> bool {\n    let score = item.score();\n    let threshold = THRESHOLD_B;\n    score > threshold\n}\n";

        let fp1 = make_fp_with_content(
            "src/core/a.rs",
            content_a,
            &[("compute_fixability", "hash_a")],
            &[("compute_fixability", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/b.rs",
            content_b,
            &[("compute_fixability", "hash_b")],
            &[("compute_fixability", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert_eq!(
            findings.len(),
            2,
            "real near-duplicates (non-idiomatic name, multi-line body, distinct body hashes) must still be flagged",
        );
        assert!(findings
            .iter()
            .all(|f| f.description.contains("compute_fixability")));
    }
}

// ========================================================================
// Intra-method duplication tests
// ========================================================================

mod intra_method {
    use super::*;

    #[test]
    fn intra_method_detects_duplicated_block() {
        // Simulate a merge artifact: same 5-line block appears twice
        let content = "<?php\nclass PipelineSteps {\n    public function handle_update( $request ) {\n        $config = array();\n        $has_provider = $request->has_param( 'provider' );\n        $has_model = $request->has_param( 'model' );\n        $has_prompt = $request->has_param( 'system_prompt' );\n        $has_disabled = $request->has_param( 'disabled_tools' );\n        $has_key = $request->has_param( 'ai_api_key' );\n\n        if ( $has_provider ) {\n            $config['provider'] = sanitize_text_field( $request->get_param( 'provider' ) );\n        }\n\n        $has_provider = $request->has_param( 'provider' );\n        $has_model = $request->has_param( 'model' );\n        $has_prompt = $request->has_param( 'system_prompt' );\n        $has_disabled = $request->has_param( 'disabled_tools' );\n        $has_key = $request->has_param( 'ai_api_key' );\n\n        if ( $has_provider ) {\n            $config['provider'] = sanitize_text_field( $request->get_param( 'provider' ) );\n        }\n\n        return $config;\n    }\n}\n";

        let mut fp = make_fingerprint(
            "inc/Api/Pipelines/PipelineSteps.php",
            &["handle_update"],
            &[],
        );
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);

        assert!(
            !findings.is_empty(),
            "Should detect duplicated block within handle_update"
        );
        assert!(findings[0].kind == AuditFinding::IntraMethodDuplicate);
        assert!(findings[0].description.contains("handle_update"));
    }

    #[test]
    fn intra_method_no_false_positive_on_unique_code() {
        let content = "<?php\nclass Handler {\n    public function process( $data ) {\n        $name = sanitize_text_field( $data['name'] );\n        $email = sanitize_email( $data['email'] );\n        $phone = sanitize_text_field( $data['phone'] );\n        $address = sanitize_text_field( $data['address'] );\n        $city = sanitize_text_field( $data['city'] );\n\n        $result = $this->save( $name, $email );\n        $this->notify( $result );\n        $this->log_action( $result );\n        $this->update_cache( $result );\n        $this->send_confirmation( $email );\n\n        return $result;\n    }\n}\n";

        let mut fp = make_fingerprint("inc/Handler.php", &["process"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            findings.is_empty(),
            "Unique code should not trigger intra-method duplication"
        );
    }

    #[test]
    fn intra_method_skips_short_methods() {
        let content = "fn short() {\n    let a = 1;\n    let b = 2;\n    let c = a + b;\n    println!(\"{}\", c);\n}\n";

        let mut fp = make_fingerprint("src/short.rs", &["short"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(findings.is_empty(), "Short methods should be skipped");
    }

    #[test]
    fn intra_method_rust_function_duplicated_block() {
        let content = "fn process_items(items: &[Item]) -> Vec<Result> {\n    let mut results = Vec::new();\n    let config = load_config();\n    let validator = Validator::new(&config);\n    let processor = Processor::new(&config);\n    let output = processor.run(&items[0]);\n\n    results.push(output);\n\n    let config = load_config();\n    let validator = Validator::new(&config);\n    let processor = Processor::new(&config);\n    let output = processor.run(&items[0]);\n\n    results.push(output);\n\n    results\n}\n";

        let mut fp = make_fingerprint("src/core/pipeline.rs", &["process_items"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            !findings.is_empty(),
            "Should detect duplicated block in Rust function"
        );
    }

    #[test]
    fn intra_method_ignores_match_arm_tail_scaffolding() {
        // Sibling dispatch arms in a `run_*` match share a boilerplate tail:
        //   )?;
        //   Ok((Variant(output), 0))
        //   }
        //   OtherArm::Name { ... } => {
        //
        // After normalization these look like 5+ identical lines across arms,
        // but they're Rust syntax, not duplicated logic. The scaffolding
        // filter should suppress the finding.
        //
        // Each arm body here is intentionally one unique line plus the
        // scaffolding tail — so the only thing that repeats is scaffolding.
        let content = "\
fn run_pr(args: PrArgs) -> Result {
    match args.command {
        PrCommand::Create {
            comp_create,
        } => {
            do_create_thing(comp_create);
            Ok((GitCommandOutput::Pr(output), 0))
        }
        PrCommand::Edit {
            comp_edit,
        } => {
            do_edit_thing(comp_edit);
            Ok((GitCommandOutput::Pr(output), 0))
        }
        PrCommand::Comment {
            comp_comment,
        } => {
            do_comment_thing(comp_comment);
            Ok((GitCommandOutput::Pr(output), 0))
        }
    }
}
";
        let mut fp = make_fingerprint("src/commands/git.rs", &["run_pr"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
        findings.is_empty(),
        "Match-arm tail scaffolding should not be flagged as duplication; got {} finding(s): {:?}",
        findings.len(),
        findings.iter().map(|f| &f.description).collect::<Vec<_>>()
    );
    }

    #[test]
    fn intra_method_still_flags_real_duplication_with_scaffolding_tails() {
        // If the repeated block contains real logic (a `let` + a call that
        // isn't an Ok/Err wrapper), we should still flag it even when it's
        // surrounded by structural lines.
        let content = "\
fn process_twice() -> Result {
    let items = load_items()?;
    let validator = Validator::new();
    let processor = Processor::new();
    let output = processor.run(&items);
    save_output(&output)?;

    let items = load_items()?;
    let validator = Validator::new();
    let processor = Processor::new();
    let output = processor.run(&items);
    save_output(&output)?;

    Ok(())
}
";
        let mut fp = make_fingerprint("src/core/pipeline.rs", &["process_twice"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            !findings.is_empty(),
            "Real duplication with logic lines should still be detected"
        );
    }

    #[test]
    fn intra_method_ignores_complementary_output_dto_tails() {
        let content = r#"
fn show(builtin: bool) -> CmdResult<ConfigOutput> {
    if builtin {
        Ok((
            ConfigOutput {
                command: "config.show".to_string(),
                defaults: Some(defaults::builtin_defaults()),
                config: None,
                path: None,
                exists: None,
                pointer: None,
                value: None,
                deleted: None,
            },
            0,
        ))
    } else {
        let config = defaults::load_config();
        Ok((
            ConfigOutput {
                command: "config.show".to_string(),
                config: Some(config),
                defaults: None,
                path: None,
                exists: None,
                pointer: None,
                value: None,
                deleted: None,
            },
            0,
        ))
    }
}
"#;
        let mut fp = make_fingerprint("src/commands/config.rs", &["show"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            findings.is_empty(),
            "Complementary DTO literal tails should not be flagged: {:?}",
            findings
                .iter()
                .map(|f| f.description.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn intra_method_ignores_repeated_error_envelopes() {
        let content = r#"
fn write_file_atomic(path: &Path, content: &str, operation: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::internal_io(
            format!("Invalid path: {}", path.display()),
            Some(operation.to_string()),
        )
    })?;

    let filename = path.file_name().ok_or_else(|| {
        Error::internal_io(
            format!("Invalid path: {}", path.display()),
            Some(operation.to_string()),
        )
    })?;

    let tmp_path = parent.join(format!("{}.tmp", filename.to_string_lossy()));
    write_tmp(tmp_path, content)
}
"#;
        let mut fp = make_fingerprint(
            "src/core/engine/local_files.rs",
            &["write_file_atomic"],
            &[],
        );
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            findings.is_empty(),
            "Repeated error envelopes should not be flagged: {:?}",
            findings
                .iter()
                .map(|f| f.description.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn intra_method_ignores_short_sibling_branch_repetition() {
        let content = r#"
fn resolve_effective_glob(args: &Args, component: &Component) -> Result<Option<String>> {
    if args.changed_only {
        let changed_files = git::working_tree_changes(&component.local_path)?;
        if changed_files.is_empty() {
            println!("No files in working tree changes");
            return Ok(Some(String::new()));
        }

        let abs_files: Vec<String> = changed_files
            .iter()
            .map(|f| format!("{}/{}", component.local_path, f))
            .collect();

        if abs_files.len() == 1 {
            Ok(Some(abs_files[0].clone()))
        } else {
            Ok(Some(format!("{{{}}}", abs_files.join(","))))
        }
    } else if let Some(ref git_ref) = args.changed_since {
        let changed_files = git::get_files_changed_since(&component.local_path, git_ref)?;
        if changed_files.is_empty() {
            println!("No files changed since {}", git_ref);
            return Ok(Some(String::new()));
        }

        let abs_files: Vec<String> = changed_files
            .iter()
            .map(|f| format!("{}/{}", component.local_path, f))
            .collect();

        if abs_files.len() == 1 {
            Ok(Some(abs_files[0].clone()))
        } else {
            Ok(Some(format!("{{{}}}", abs_files.join(","))))
        }
    } else {
        Ok(args.glob.clone())
    }
}
"#;
        let mut fp = make_fingerprint(
            "src/core/extension/lint/run.rs",
            &["resolve_effective_glob"],
            &[],
        );
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            findings.is_empty(),
            "Short sibling-branch repetition should not be flagged: {:?}",
            findings
                .iter()
                .map(|f| f.description.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn intra_method_ignores_repeated_multiline_call_argument_tails() {
        let content = r#"
fn env(extension: &Extension, local_path: &Path) -> Result<()> {
    if let Some(detected) = run_component_env_detector(extension, local_path)? {
        apply_component_env_detector_output(
            detected,
            &mut node_version,
            &mut node_source,
            &mut php_version,
            &mut php_source,
        );
    }

    if let Some(runtime) = extension.runtime.as_ref() {
        apply_extension_runtime_requirements(
            ext_id,
            runtime,
            &mut node_version,
            &mut node_source,
            &mut php_version,
            &mut php_source,
        );
    }
}
"#;
        let mut fp = make_fingerprint("src/commands/component.rs", &["env"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            findings.is_empty(),
            "Repeated argument tails on different calls should not be flagged: {:?}",
            findings
                .iter()
                .map(|f| f.description.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn intra_method_ignores_repeated_match_arm_result_shapes() {
        let content = r#"
fn search(mode: SearchMode, line: &str, term: &str) {
    match mode {
        SearchMode::Boundary => {
            for pos in find_boundary_matches(line, term) {
                results.push(Match {
                    file: relative.clone(),
                    line: line_num + 1,
                    column: pos + 1,
                    matched: term.to_string(),
                    context: line.to_string(),
                });
            }
        }
        SearchMode::Literal => {
            for pos in find_literal_matches(line, term) {
                results.push(Match {
                    file: relative.clone(),
                    line: line_num + 1,
                    column: pos + 1,
                    matched: term.to_string(),
                    context: line.to_string(),
                });
            }
        }
    }
}
"#;
        let mut fp = make_fingerprint("src/core/engine/codebase_scan.rs", &["search"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            findings.is_empty(),
            "Repeated match-arm result shapes should not be flagged: {:?}",
            findings
                .iter()
                .map(|f| f.description.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn intra_method_still_flags_adjacent_logic_copy_paste() {
        let content = r#"
fn rebuild_twice(items: &[Item]) -> Result<()> {
    let config = load_config()?;
    let validator = Validator::new(&config);
    let processor = Processor::new(&config);
    let output = processor.run(&items[0]);
    save_output(&output)?;

    let config = load_config()?;
    let validator = Validator::new(&config);
    let processor = Processor::new(&config);
    let output = processor.run(&items[0]);
    save_output(&output)?;

    Ok(())
}
"#;
        let mut fp = make_fingerprint("src/core/pipeline.rs", &["rebuild_twice"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            !findings.is_empty(),
            "Adjacent repeated logic should still be reported"
        );
    }

    #[test]
    fn scaffolding_line_classifier() {
        // Positive cases (structural).
        for line in &[
            "}",
            "},",
            ")",
            ")?;",
            "))",
            "))?",
            "path,",
            "component_id,",
            "path",
            "ok((gitcommandoutput::pr(output), 0))",
            "ok(output)",
            "err(e)",
            "none",
            "} => {",
            "_ => {",
            "foo => {",
        ] {
            assert!(
                is_scaffolding_line(line),
                "Expected scaffolding: {:?}",
                line
            );
        }

        // Negative cases (real logic).
        for line in &[
            "let x = foo();",
            "x = y + 1",
            "if x.is_empty() {",
            "for item in items {",
            "compute(&items)?",
            ".stdout(std::process::stdio::null())",
        ] {
            assert!(
                !is_scaffolding_line(line) || has_logic_signal(line),
                "Expected logic: {:?}",
                line
            );
        }
    }

    #[test]
    fn logic_signal_detector() {
        assert!(has_logic_signal("let x = foo();"));
        assert!(has_logic_signal("x = 1"));
        assert!(has_logic_signal("if cond {"));
        assert!(has_logic_signal("for x in y {"));
        assert!(has_logic_signal("while true {"));
        assert!(has_logic_signal("match thing {"));
        assert!(has_logic_signal("return x"));
        assert!(has_logic_signal(".stdout(something())"));
        assert!(has_logic_signal("compute(&items)"));

        // Return wrappers are NOT logic (they're structural tail expressions).
        assert!(!has_logic_signal("ok(())"));
        assert!(!has_logic_signal("ok((output, 0))"));
        assert!(!has_logic_signal("err(e)"));
        assert!(!has_logic_signal("some(x)"));
        assert!(!has_logic_signal("none"));

        // Pure punctuation is not logic.
        assert!(!has_logic_signal("}"));
        assert!(!has_logic_signal(")?;"));
    }

    #[test]
    fn find_method_body_php() {
        let content =
            "<?php\nclass Foo {\n    public function bar() {\n        return 1;\n    }\n}\n";
        let lines: Vec<&str> = content.lines().collect();
        let result = find_method_body(&lines, "bar");
        assert!(result.is_some());
        let (open, close) = result.unwrap();
        assert!(lines[open].contains('{'));
        assert!(lines[close].contains('}'));
    }

    #[test]
    fn find_method_body_rust() {
        let content = "fn hello() {\n    println!(\"hi\");\n}\n";
        let lines: Vec<&str> = content.lines().collect();
        let result = find_method_body(&lines, "hello");
        assert!(result.is_some());
    }

    #[test]
    fn find_method_body_missing() {
        let content = "fn other() {\n    println!(\"hi\");\n}\n";
        let lines: Vec<&str> = content.lines().collect();
        let result = find_method_body(&lines, "nonexistent");
        assert!(result.is_none());
    }
}

mod parallel;
