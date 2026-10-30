use super::*;
use homeboy_core::component::ScopedExtensionConfig;
use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-lint-fix-{name}-{nanos}"))
}

use homeboy_test_support::git::run as run_git;

fn routed_component(root: &Path, extension_id: &str) -> Component {
    let mut component = Component::new(
        "fixture".to_string(),
        root.to_string_lossy().to_string(),
        String::new(),
        None,
    );
    component.extensions = Some(HashMap::from([(
        extension_id.to_string(),
        ScopedExtensionConfig::default(),
    )]));
    component
}

fn request(component: Component, root: &Path) -> LintFixRequest {
    LintFixRequest {
        component,
        root: root.to_path_buf(),
        settings: Vec::new(),
        selected_files: None,
        summary: false,
        file: None,
        glob: None,
        sniff_filters: LintSniffFilters::default(),
        category: None,
        changed_since: None,
        force: false,
    }
}

fn write_routed_lint_extension(home: &Path, id: &str, mutates: bool) {
    let extension = home.join(".config/homeboy/extensions").join(id);
    fs::create_dir_all(&extension).unwrap();
    fs::write(
        extension.join(format!("{id}.json")),
        serde_json::json!({
            "name": "Routed lint fixture",
            "version": "0.0.0",
            "structured_sidecars": { "lint.findings": true },
            "lint": {
                "extension_script": "lint.sh",
                "changed_file_routes": [{
                    "extensions": ["fixture"],
                    "step": "fixture-fixer"
                }]
            }
        })
        .to_string(),
    )
    .unwrap();
    homeboy_test_support::write_executable(extension.join("lint.sh"), format!(
            "#!/bin/sh\nset -eu\nfile=\"$HOMEBOY_COMPONENT_PATH/src/example.fixture\"\necho \"${{HOMEBOY_FIX_ONLY:-diagnose}}:${{HOMEBOY_STEP:-all}}:${{HOMEBOY_SUMMARY_MODE:-0}}\" >> \"$HOMEBOY_COMPONENT_PATH/runner.log\"\nif [ \"${{HOMEBOY_FIX_ONLY:-}}\" = 1 ]; then\n  [ \"${{HOMEBOY_STEP:-}}\" = fixture-fixer ] || exit 91\n  {}\n  exit 0\nfi\nif grep -q unresolved \"$file\"; then\n  printf '%s\\n' \"[{{\\\"tool\\\":\\\"fixture\\\",\\\"file\\\":\\\"$file\\\",\\\"message\\\":\\\"unresolved fixture finding\\\",\\\"rule\\\":\\\"fixture.rule\\\",\\\"fixable\\\":true}}]\" > \"$HOMEBOY_LINT_FINDINGS_FILE\"\nelse\n  printf '%s\\n' '[]' > \"$HOMEBOY_LINT_FINDINGS_FILE\"\nfi\n",
            if mutates { "printf 'resolved\\n' > \"$file\"" } else { ":" }
        ), 0o755)
    .unwrap();
}

fn write_multi_producer_lint_extension(home: &Path, id: &str) {
    let extension = home.join(".config/homeboy/extensions").join(id);
    fs::create_dir_all(&extension).unwrap();
    fs::write(
        extension.join(format!("{id}.json")),
        serde_json::json!({
            "name": "Multi producer lint fixture",
            "version": "0.0.0",
            "structured_sidecars": { "lint.findings": true },
            "lint": {
                "extension_script": "lint.sh",
                "changed_file_routes": [
                    { "extensions": ["fixture"], "step": "fixture-fixer" },
                    { "extensions": ["other"], "step": "other-fixer" }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();
    homeboy_test_support::write_executable(extension.join("lint.sh"), "#!/bin/sh\nset -eu\nfixture=\"$HOMEBOY_COMPONENT_PATH/src/example.fixture\"\nother=\"$HOMEBOY_COMPONENT_PATH/src/example.other\"\necho \"${HOMEBOY_FIX_ONLY:-diagnose}:${HOMEBOY_STEP:-all}\" >> \"$HOMEBOY_COMPONENT_PATH/runner.log\"\nif [ \"${HOMEBOY_FIX_ONLY:-}\" = 1 ]; then\n  case \"$HOMEBOY_STEP\" in\n    fixture-fixer) printf 'resolved\\n' > \"$fixture\" ;;\n    other-fixer) printf 'resolved\\n' > \"$other\" ;;\n    *) exit 91 ;;\n  esac\n  exit 0\nfi\nif grep -q unresolved \"$fixture\" || grep -q unresolved \"$other\"; then\n  printf '%s\\n' '[{\"tool\":\"fixture\",\"file\":\"src/example.fixture\",\"message\":\"fixture finding\",\"rule\":\"fixture.rule\",\"fixable\":true},{\"tool\":\"other\",\"file\":\"src/example.other\",\"message\":\"other finding\",\"rule\":\"other.rule\",\"fixable\":true}]' > \"$HOMEBOY_LINT_FINDINGS_FILE\"\nelse\n  printf '%s\\n' '[]' > \"$HOMEBOY_LINT_FINDINGS_FILE\"\nfi\n", 0o755)
    .unwrap();
}

/// Commit the fixture source plus an ignore rule for the runner log, so the
/// fixture's own bookkeeping never reads as a dirty working tree.
fn init_routed_lint_repo(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/example.fixture"), "unresolved\n").unwrap();
    fs::write(root.join(".gitignore"), "runner.log\n").unwrap();
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "test"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "fixture"]);
}

#[test]
fn routes_glob_scoped_finding_to_declared_fixer() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let root = tmp_dir("glob-routed-fix");
        init_routed_lint_repo(&root);
        write_routed_lint_extension(home.path(), "routed-fixture", true);

        let run = run_lint_fix(LintFixRequest {
            glob: Some("src/*.{fixture,other}".to_string()),
            ..request(routed_component(&root, "routed-fixture"), &root)
        })
        .expect("routed fixer should resolve glob-scoped finding");

        assert_eq!(
            fs::read_to_string(root.join("src/example.fixture")).unwrap(),
            "resolved\n"
        );
        assert_eq!(run.changed_files, vec!["src/example.fixture"]);
        assert!(run.applied);
        assert_eq!(run.files_modified, 1);
        assert_eq!(
            fs::read_to_string(root.join("runner.log")).unwrap(),
            "diagnose:all:0\n1:fixture-fixer:0\ndiagnose:all:0\n"
        );
        let _ = fs::remove_dir_all(root);
    });
}

#[test]
fn summary_mode_is_forwarded_to_diagnostics_and_fixer() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let root = tmp_dir("summary-fix");
        init_routed_lint_repo(&root);
        write_routed_lint_extension(home.path(), "summary-fixture", true);

        run_lint_fix(LintFixRequest {
            summary: true,
            ..request(routed_component(&root, "summary-fixture"), &root)
        })
        .expect("summary fix should succeed");

        assert_eq!(
            fs::read_to_string(root.join("runner.log")).unwrap(),
            "diagnose:all:1\n1:fixture-fixer:1\ndiagnose:all:1\n"
        );
        let _ = fs::remove_dir_all(root);
    });
}

#[test]
fn records_multi_producer_fix_lifecycle_through_verification() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let root = tmp_dir("multi-producer-fix");
        init_routed_lint_repo(&root);
        fs::write(root.join("src/example.other"), "unresolved\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "other fixture"]);
        write_multi_producer_lint_extension(home.path(), "multi-producer-fixture");
        let run_dir = RunDir::create().unwrap();

        let outcome = fix_lint_findings(
            &request(routed_component(&root, "multi-producer-fixture"), &root),
            None,
            &run_dir,
        )
        .expect("both producers and post-fix verification should complete");

        assert_eq!(
            outcome.changed_files,
            vec!["src/example.fixture", "src/example.other"]
        );
        let ledger =
            homeboy_core::validation_progress::ValidationProgressLedger::read_from_run_dir(
                &run_dir,
            )
            .expect("fix lifecycle ledger");
        assert_eq!(ledger.status, "passed");
        assert_eq!(ledger.command_count, 4);
        assert_eq!(ledger.commands[0].label, "lint diagnostics");
        assert_eq!(ledger.commands[1].label, "lint fixer fixture-fixer");
        assert_eq!(ledger.commands[2].label, "lint fixer other-fixer");
        assert_eq!(ledger.commands[3].label, "post-fix verification");
        assert!(ledger
            .commands
            .iter()
            .all(|command| command.finished_at.is_some()));
        assert!(ledger.active_command.is_none());
        assert_eq!(
            fs::read_to_string(root.join("runner.log")).unwrap(),
            "diagnose:all\n1:fixture-fixer\n1:other-fixer\ndiagnose:all\n"
        );
        let _ = fs::remove_dir_all(root);
    });
}

#[test]
fn unresolved_diagnostics_after_fixer_fail_the_fix() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let root = tmp_dir("glob-unresolved-fix");
        init_routed_lint_repo(&root);
        write_routed_lint_extension(home.path(), "unresolved-fixture", false);

        let error = run_lint_fix(LintFixRequest {
            glob: Some("src/*.{fixture,other}".to_string()),
            ..request(routed_component(&root, "unresolved-fixture"), &root)
        })
        .expect_err("unresolved diagnostic must fail instead of reporting autofix success");

        assert!(error.message.contains("Lint fix left 1 finding(s)"));
        assert!(error.message.contains("unresolved fixture finding"));
        assert_eq!(
            fs::read_to_string(root.join("runner.log")).unwrap(),
            "diagnose:all:0\n1:fixture-fixer:0\ndiagnose:all:0\n"
        );
        let _ = fs::remove_dir_all(root);
    });
}

#[test]
fn empty_selected_files_does_not_fall_back_to_broad_lint() {
    let root = tmp_dir("lint-empty-selected-files");
    fs::create_dir_all(&root).unwrap();
    let run_dir = RunDir::create().unwrap();
    let component = Component::new(
        "component".to_string(),
        root.to_string_lossy().to_string(),
        String::new(),
        None,
    );

    let outcome = fix_lint_findings(
        &LintFixRequest {
            selected_files: Some(Vec::new()),
            ..request(component, &root)
        },
        None,
        &run_dir,
    )
    .expect("empty selected files should be a clean no-op");

    assert!(outcome.changed_files.is_empty());
    assert!(outcome.fix_results.is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn broad_fix_refuses_a_dirty_working_tree_unless_forced() {
    let root = tmp_dir("dirty-broad-fix");
    init_routed_lint_repo(&root);
    fs::write(root.join("src/example.fixture"), "manual edit\n").unwrap();
    let component = routed_component(&root, "unused");

    let error = ensure_clean_enough_for_write(&request(component.clone(), &root))
        .expect_err("dirty tree must block a broad fix");
    assert!(error
        .to_string()
        .contains("Working tree has uncommitted changes"));

    ensure_clean_enough_for_write(&LintFixRequest {
        force: true,
        ..request(component.clone(), &root)
    })
    .expect("--force bypasses the dirty-tree guard");
    ensure_clean_enough_for_write(&LintFixRequest {
        selected_files: Some(vec!["src/example.fixture".to_string()]),
        ..request(component, &root)
    })
    .expect("a bounded selection may fix a dirty tree");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn refuses_a_foreign_live_worktree_holder_before_writing() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let root = tmp_dir("foreign-held-worktree");
        init_routed_lint_repo(&root);
        let source = root.join("src/example.fixture");
        let original = fs::read_to_string(&source).expect("read original source");
        homeboy_core::worktree::record_active_for_test("fixture@foreign-held", &root);
        let record = homeboy_core::worktree::resolve("fixture@foreign-held")
            .expect("resolve managed worktree");
        let claims = homeboy_core::workspace_claim::WorkspaceClaimStore::from_environment()
            .expect("workspace authority");
        claims
            .register_owner(
                record.effective_workspace_identity().expect("identity"),
                "ses_foreign_13748",
                300_000,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("current time")
                    .as_millis() as u64,
            )
            .expect("register foreign holder");

        let error = run_lint_fix(request(routed_component(&root, "unused"), &root))
            .expect_err("foreign live holder must protect the checkout");

        let message = error.to_string();
        assert!(message.contains("ses_foreign_13748"), "{message}");
        assert!(message.contains("--force"), "{message}");
        assert_eq!(
            fs::read_to_string(&source).expect("read protected source"),
            original,
            "the fixer must leave the held checkout untouched"
        );
        let _ = fs::remove_dir_all(&root);
    });
}

#[test]
fn formatter_failure_is_a_fix_failure() {
    let error = require_successful_format(homeboy_core::engine::format_write::FormatResult {
        success: false,
        command: Some("format.sh src/file.php".to_string()),
        output: Some("phpcbf failed".to_string()),
        files_in_scope: 1,
    })
    .expect_err("formatter failure should fail the fix");
    assert!(error.to_string().contains("post-write formatter"));
    assert!(error.to_string().contains("phpcbf failed"));
}

#[test]
fn remaining_findings_fail_the_fix() {
    let findings = vec![HomeboyFinding::builder("lint", "Tabs must be used")
        .rule("WordPress.WhiteSpace.DisallowSpaceIndent")
        .file("inc/Demo.php")
        .fixable(true)
        .build()];

    let error = reject_remaining_findings(&findings)
        .expect_err("remaining lint findings should fail fix mode");
    assert!(error.to_string().contains("Lint fix left 1 finding"));
    assert!(error.to_string().contains("inc/Demo.php"));
    reject_remaining_findings(&[]).expect("clean follow-up diagnostics should pass");
}

fn fixable_finding(file: &str, fixable: bool) -> HomeboyFinding {
    HomeboyFinding::builder("phpcs", "WhiteSpace")
        .file(file)
        .fixable(fixable)
        .build()
}

#[test]
fn advertised_fixable_with_no_local_changes_fails() {
    let findings = vec![
        fixable_finding("src/a.php", true),
        fixable_finding("src/b.php", true),
    ];
    let error = reject_unapplied_fixable_findings(&findings, &[])
        .expect_err("advertised-fixable + no local change must fail");
    assert!(
        error
            .message
            .contains("2 auto-fixable finding(s), but the fixer did not modify the local worktree"),
        "error must report the discrepancy: {error}"
    );
    assert!(error
        .details
        .to_string()
        .contains("homeboy review lint <component>"));

    reject_unapplied_fixable_findings(&findings[..1], &["src/a.php".to_string()]).unwrap();
    reject_unapplied_fixable_findings(&[fixable_finding("src/a.php", false)], &[]).unwrap();
}

#[test]
fn fix_results_summarize_rules_and_primitives() {
    let fixes = vec![
        FixApplied {
            file: "src/a.php".to_string(),
            rule: "phpcbf:WhiteSpace".to_string(),
            primitive: None,
        },
        FixApplied {
            file: "src/b.php".to_string(),
            rule: "phpcbf:WhiteSpace".to_string(),
            primitive: Some("rewrite".to_string()),
        },
    ];

    let summary = summarize_fix_results(&fixes).expect("summary");
    assert_eq!(summary.fixes_applied, 2);
    assert_eq!(summary.files_modified, 2);
    assert_eq!(summary.rules[0].rule, "phpcbf:WhiteSpace");
    assert_eq!(summary.rules[0].count, 2);
    assert_eq!(summary.primitives[0].primitive, "rewrite");
    assert!(summarize_fix_results(&[]).is_none());
}
