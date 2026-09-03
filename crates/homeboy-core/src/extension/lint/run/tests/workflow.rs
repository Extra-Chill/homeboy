use super::super::workflow::{
    finish_scoped_lint_run_dir, run_main_lint_workflow, run_self_check_lint_workflow,
};
use super::{component, lint_args};
use homeboy_core::component::{Component, ComponentScriptsConfig, ScopedExtensionConfig};
use homeboy_core::engine::run_dir::RunDir;
use std::collections::HashMap;
use std::path::Path;

#[test]
fn test_run_self_check_lint_workflow() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("lint.sh"), "printf lint-ok\n")
        .expect("script should be written");

    let mut component = Component::new(
        "fixture".to_string(),
        dir.path().to_string_lossy().to_string(),
        "".to_string(),
        None,
    );
    component.scripts = Some(ComponentScriptsConfig {
        lint: vec!["sh lint.sh".to_string()],
        test: Vec::new(),
        build: Vec::new(),
        bench: Vec::new(),
        fuzz: Vec::new(),
        trace: Vec::new(),
        deps: Vec::new(),
    });

    let result = run_self_check_lint_workflow(&component, dir.path(), "fixture".to_string(), false)
        .expect("lint self-check should run");

    assert_eq!(result.status, "passed");
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.component, "fixture");
}

#[test]
fn test_run_main_lint_workflow() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .expect("git init should run");
    let run_dir = RunDir::create().expect("run dir");
    let mut args = lint_args();
    args.changed_only = true;

    let result = run_main_lint_workflow(
        &component(&dir.path().to_string_lossy()),
        dir.path(),
        args,
        &run_dir,
    )
    .expect("unchanged git repo should skip lint runner");

    assert_eq!(result.status, "passed");
    assert_eq!(result.exit_code, 0);
    assert!(result.findings.is_none());
}

fn routed_lint_component(home: &Path, source: &Path, script: &str) -> Component {
    let extension_dir = home.join(".config/homeboy/extensions/routed-lint-fixture");
    std::fs::create_dir_all(&extension_dir).expect("extension dir");
    std::fs::write(
        extension_dir.join("routed-lint-fixture.json"),
        r#"{
            "name":"Routed lint fixture",
            "version":"1.0.0",
            "structured_sidecars":{
                "lint.findings":true,
                "lint.producers":true
            },
            "lint":{
                "extension_script":"lint.sh",
                "changed_file_routes":[
                    {"extensions":["php"],"step":"php"},
                    {"extensions":["js"],"step":"js"}
                ]
            }
        }"#,
    )
    .expect("extension manifest");
    std::fs::write(extension_dir.join("lint.sh"), script).expect("lint script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script_path = extension_dir.join("lint.sh");
        let mut permissions = std::fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script_path, permissions).expect("executable script");
    }

    Component {
        id: "fixture".to_string(),
        local_path: source.to_string_lossy().to_string(),
        extensions: Some(HashMap::from([(
            "routed-lint-fixture".to_string(),
            ScopedExtensionConfig::default(),
        )])),
        ..Default::default()
    }
}

fn routed_lint_args() -> super::super::types::LintRunWorkflowArgs {
    let mut args = lint_args();
    args.changed_since = Some("v1.0.0".to_string());
    args.precomputed_changed_files =
        Some(vec!["legacy.php".to_string(), "assets/app.js".to_string()]);
    args.json_summary = true;
    args
}

fn producer_sidecar_paths(
    workflow: &super::super::types::LintRunWorkflowResult,
) -> Vec<std::path::PathBuf> {
    workflow
        .producer_summaries
        .iter()
        .filter_map(|producer| producer.source.as_ref()?.path.as_ref())
        .map(std::path::PathBuf::from)
        .collect()
}

#[test]
fn multi_route_lint_aggregates_later_route_findings() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
if [ "$HOMEBOY_STEP" = "php" ]; then
  printf '[]' > "$HOMEBOY_LINT_FINDINGS_FILE"
  exit 0
fi
printf '[{"tool":"eslint","message":"later route finding","fingerprint":"later","file":"assets/app.js"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
        );
        let run_dir = RunDir::create().expect("run dir");

        let workflow =
            run_main_lint_workflow(&component, source.path(), routed_lint_args(), &run_dir)
                .expect("workflow result");

        assert_eq!(workflow.status, "failed");
        assert_eq!(workflow.exit_code, 1);
        let findings = workflow.findings.as_ref().expect("findings");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].message, "later route finding");
        assert_eq!(workflow.producer_summaries[1].step.as_deref(), Some("js"));
        assert!(producer_sidecar_paths(&workflow)
            .iter()
            .all(|path| path.is_file()));
    });
}

#[test]
fn full_scope_legacy_baseline_is_compared_with_legacy_provenance() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let finding = homeboy_core::finding::HomeboyFinding::builder("eslint", "known finding")
            .fingerprint("known")
            .build();
        crate::extension::lint::baseline::save_baseline(source.path(), "legacy", &[finding])
            .expect("save legacy baseline");
        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
printf '[{"tool":"eslint","message":"known finding","fingerprint":"known","file":"src/lib.rs"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
        );

        let workflow = run_main_lint_workflow(
            &component,
            source.path(),
            lint_args(),
            &RunDir::create().expect("run dir"),
        )
        .expect("workflow result");

        assert_eq!(workflow.status, "passed");
        assert_eq!(workflow.exit_code, 0);
        let provenance = workflow
            .baseline_provenance
            .as_ref()
            .expect("baseline provenance");
        assert!(provenance.compared);
        assert_eq!(
            provenance.resolution,
            crate::extension::lint::baseline::LintBaselineResolution::LegacyFull
        );
        assert_eq!(provenance.baseline_key, "lint");
    });
}

#[test]
fn empty_legacy_full_baseline_is_incomparable_instead_of_new_drift() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        crate::extension::lint::baseline::save_baseline(source.path(), "legacy", &[])
            .expect("save empty legacy baseline");
        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
printf '[{"tool":"phpcs","message":"standing warning","fingerprint":"standing","file":"src/lib.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
printf '[{"tool":"phpcs","status":"passed","finding_count":1}]' > "$HOMEBOY_LINT_PRODUCERS_FILE"
exit 1
"#,
        );

        let workflow = run_main_lint_workflow(
            &component,
            source.path(),
            lint_args(),
            &RunDir::create().expect("run dir"),
        )
        .expect("workflow result");

        assert_eq!(workflow.status, "passed");
        assert_eq!(workflow.exit_code, 0);
        assert!(workflow.baseline_comparison.is_none());
        let provenance = workflow
            .baseline_provenance
            .as_ref()
            .expect("baseline provenance");
        assert!(!provenance.compared);
        assert_eq!(provenance.baseline_key, "lint");
        assert_eq!(
            provenance.resolution,
            crate::extension::lint::baseline::LintBaselineResolution::LegacyEmptyIncomparable
        );
    });
}

#[test]
fn scoped_empty_full_baseline_still_blocks_a_genuine_increase() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let provenance = crate::extension::lint::baseline::LintBaselineProvenance::new(
            Vec::new(),
            vec!["phpcs".to_string()],
            "full",
            None,
            false,
            None,
            None,
        );
        crate::extension::lint::baseline::save_baseline_for_scope(
            source.path(),
            "scoped",
            &[],
            Some(&provenance),
        )
        .expect("save scoped empty baseline");
        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
printf '[{"tool":"phpcs","message":"introduced warning","fingerprint":"introduced","file":"src/lib.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
printf '[{"tool":"phpcs","status":"passed","finding_count":1}]' > "$HOMEBOY_LINT_PRODUCERS_FILE"
exit 1
"#,
        );

        let workflow = run_main_lint_workflow(
            &component,
            source.path(),
            lint_args(),
            &RunDir::create().expect("run dir"),
        )
        .expect("workflow result");

        assert_eq!(workflow.status, "failed");
        assert_eq!(workflow.exit_code, 1);
        assert!(workflow
            .baseline_comparison
            .as_ref()
            .is_some_and(|comparison| comparison.new_items.len() == 1));
        assert_eq!(
            workflow
                .baseline_provenance
                .as_ref()
                .map(|provenance| &provenance.resolution),
            Some(&crate::extension::lint::baseline::LintBaselineResolution::Scoped)
        );
    });
}

#[test]
fn accepted_pr_finding_stays_accepted_on_full_default_branch_run() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let run_git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(source.path())
                .output()
                .expect("git command");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "homeboy@example.com"]);
        run_git(&["config", "user.name", "Homeboy Test"]);
        std::fs::write(
            source.path().join("legacy.php"),
            "<?php // standing warning\n",
        )
        .expect("base source");
        run_git(&["add", "."]);
        run_git(&["commit", "-q", "-m", "base"]);
        run_git(&["branch", "baseline"]);
        std::fs::write(
            source.path().join("legacy.php"),
            "<?php // standing warning\n// harmless change\n",
        )
        .expect("candidate source");
        run_git(&["add", "."]);
        run_git(&["commit", "-q", "-m", "candidate"]);
        crate::extension::lint::baseline::save_baseline(source.path(), "legacy", &[])
            .expect("save empty legacy baseline");
        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
printf '[{"tool":"phpcs","message":"standing warning","fingerprint":"standing","file":"legacy.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
printf '[{"tool":"phpcs","status":"passed","finding_count":1}]' > "$HOMEBOY_LINT_PRODUCERS_FILE"
exit 1
"#,
        );

        let mut pr_args = lint_args();
        pr_args.changed_since = Some("baseline".to_string());
        pr_args.precomputed_changed_files = Some(vec!["legacy.php".to_string()]);
        let pr = run_main_lint_workflow(
            &component,
            source.path(),
            pr_args,
            &RunDir::create().expect("PR run dir"),
        )
        .expect("PR workflow result");
        let push = run_main_lint_workflow(
            &component,
            source.path(),
            lint_args(),
            &RunDir::create().expect("push run dir"),
        )
        .expect("push workflow result");

        assert_eq!((pr.status.as_str(), pr.exit_code), ("passed", 0));
        assert_eq!((push.status.as_str(), push.exit_code), ("passed", 0));
        assert_eq!(
            pr.baseline_provenance
                .as_ref()
                .map(|provenance| &provenance.resolution),
            Some(&crate::extension::lint::baseline::LintBaselineResolution::GitBase)
        );
        assert_eq!(
            push.baseline_provenance
                .as_ref()
                .map(|provenance| &provenance.resolution),
            Some(
                &crate::extension::lint::baseline::LintBaselineResolution::LegacyEmptyIncomparable
            )
        );
    });
}

#[test]
fn scoped_zero_findings_do_not_compare_unrelated_legacy_baseline() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let run_git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(source.path())
                .output()
                .expect("git command");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "homeboy@example.com"]);
        run_git(&["config", "user.name", "Homeboy Test"]);
        std::fs::write(source.path().join("legacy.php"), "<?php // base\n").expect("base source");
        std::fs::write(source.path().join("untouched.php"), "<?php // debt\n")
            .expect("untouched debt source");
        run_git(&["add", "."]);
        run_git(&["commit", "-q", "-m", "base"]);
        run_git(&["branch", "baseline"]);
        std::fs::write(source.path().join("legacy.php"), "<?php // candidate\n")
            .expect("candidate source");
        run_git(&["add", "."]);
        run_git(&["commit", "-q", "-m", "candidate"]);

        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
if grep -q candidate "$HOMEBOY_LINT_GLOB"; then
  if grep -q 'candidate new' "$HOMEBOY_LINT_GLOB"; then
    printf '[{"tool":"phpcs","message":"introduced","fingerprint":"introduced","file":"legacy.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
    exit 1
  fi
  printf '[]' > "$HOMEBOY_LINT_FINDINGS_FILE"
  printf '[{"tool":"phpcs","status":"passed","finding_count":0},{"tool":"phpstan","status":"passed","finding_count":0}]' > "$HOMEBOY_LINT_PRODUCERS_FILE"
  exit 0
fi
printf '[{"tool":"phpcs","message":"known","fingerprint":"known","file":"legacy.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
        );

        let mut pr_args = lint_args();
        pr_args.changed_since = Some("baseline".to_string());
        pr_args.precomputed_changed_files = Some(vec!["legacy.php".to_string()]);
        let pr_result = run_main_lint_workflow(
            &component,
            source.path(),
            pr_args,
            &RunDir::create().expect("PR run dir"),
        )
        .expect("PR workflow result");

        assert_eq!(pr_result.status, "passed");
        assert_eq!(pr_result.exit_code, 0);
        assert!(pr_result.findings.as_ref().is_some_and(Vec::is_empty));
        assert!(pr_result
            .baseline_comparison
            .is_some_and(|comparison| comparison.new_items.is_empty()));
        let provenance = pr_result
            .baseline_provenance
            .as_ref()
            .expect("scoped baseline provenance");
        assert!(provenance.compared);
        assert!(provenance.base_ref.is_some());
        assert_eq!(provenance.files, vec!["legacy.php"]);
        assert_eq!(provenance.tools, vec!["phpcs", "phpstan"]);
        assert_eq!(provenance.scope, "changed");
        assert!(provenance.baseline_key.starts_with("lint:"));
        assert_ne!(provenance.baseline_key, "lint");

        std::fs::write(source.path().join("legacy.php"), "<?php // candidate new\n")
            .expect("introduced candidate source");
        let mut introduced_args = lint_args();
        introduced_args.changed_since = Some("baseline".to_string());
        introduced_args.precomputed_changed_files = Some(vec!["legacy.php".to_string()]);
        let introduced = run_main_lint_workflow(
            &component,
            source.path(),
            introduced_args,
            &RunDir::create().expect("introduced finding run dir"),
        )
        .expect("introduced finding workflow result");
        assert_eq!(introduced.status, "failed");
        assert_eq!(introduced.exit_code, 1);
        assert!(introduced
            .baseline_comparison
            .is_some_and(|comparison| comparison.new_items.len() == 1));
    });
}

#[test]
fn unavailable_changed_since_baseline_does_not_suppress_matching_stored_findings() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
printf '[{"tool":"phpcs","message":"known","fingerprint":"known","file":"legacy.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
        );
        let mut seed_args = lint_args();
        seed_args.changed_since = Some("unreachable-base".to_string());
        seed_args.precomputed_changed_files = Some(vec!["legacy.php".to_string()]);
        seed_args.baseline_flags.ignore_baseline = true;
        let seed = run_main_lint_workflow(
            &component,
            source.path(),
            seed_args,
            &RunDir::create().expect("seed run dir"),
        )
        .expect("seed workflow result");
        let provenance = seed
            .baseline_provenance
            .as_ref()
            .expect("stored baseline provenance");
        crate::extension::lint::baseline::save_baseline_for_scope(
            source.path(),
            "fixture",
            seed.findings.as_deref().expect("seed findings"),
            Some(provenance),
        )
        .expect("save matching baseline");

        let mut args = lint_args();
        args.changed_since = Some("unreachable-base".to_string());
        args.precomputed_changed_files = Some(vec!["legacy.php".to_string()]);
        let result = run_main_lint_workflow(
            &component,
            source.path(),
            args,
            &RunDir::create().expect("candidate run dir"),
        )
        .expect("candidate workflow result");

        assert_eq!(result.status, "failed");
        assert_eq!(result.exit_code, 1);
        assert!(result.baseline_comparison.is_none());
        assert!(result
            .baseline_provenance
            .is_some_and(|provenance| !provenance.compared));
        assert_eq!(result.findings.as_ref().map(Vec::len), Some(1));
    });
}

#[test]
fn runner_failure_without_findings_is_an_infrastructure_error_without_autofix() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
printf '%s\n' 'toolchain unavailable' >&2
exit 127
"#,
        );
        let run_dir = RunDir::create().expect("run dir");

        let workflow =
            run_main_lint_workflow(&component, source.path(), routed_lint_args(), &run_dir)
                .expect("runner failure should normalize into a result");

        assert_eq!(workflow.status, "error");
        assert_eq!(workflow.exit_code, 127);
        assert!(workflow.infrastructure_failure);
        assert!(workflow.findings.as_ref().is_some_and(Vec::is_empty));
        assert_eq!(
            workflow
                .summary
                .as_ref()
                .map(|summary| summary.total_findings),
            Some(0)
        );
        assert!(workflow
            .producer_summaries
            .iter()
            .all(|summary| summary.finding_count == 0));
        assert!(!workflow
            .hints
            .as_ref()
            .is_some_and(|hints| hints.iter().any(|hint| hint.starts_with("Auto-fix:"))));
    });
}

#[test]
fn multi_route_failure_retains_later_success_evidence() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
if [ "$HOMEBOY_STEP" = "php" ]; then
  printf '[{"tool":"phpcs","message":"first route finding","fingerprint":"first","file":"legacy.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
  exit 1
fi
printf '[]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 0
"#,
        );
        let run_dir = RunDir::create().expect("run dir");

        let workflow =
            run_main_lint_workflow(&component, source.path(), routed_lint_args(), &run_dir)
                .expect("workflow result");

        assert_eq!(workflow.status, "failed");
        assert_eq!(workflow.findings.as_ref().map(Vec::len), Some(1));
        let paths = producer_sidecar_paths(&workflow);
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|path| path.is_file()));
    });
}

#[test]
fn successful_zero_finding_routes_initialize_and_clean_evidence() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let component = routed_lint_component(home.path(), source.path(), "#!/bin/sh\nexit 0\n");
        let run_dir = RunDir::create().expect("run dir");

        let workflow =
            run_main_lint_workflow(&component, source.path(), routed_lint_args(), &run_dir)
                .expect("workflow result");

        assert_eq!(workflow.status, "passed");
        let paths = producer_sidecar_paths(&workflow);
        assert_eq!(paths.len(), 2);
        assert!(
            paths[0].is_file(),
            "primary run evidence remains caller-owned"
        );
        assert!(!paths[1].exists(), "successful child route must be cleaned");
    });
}

#[test]
fn nested_component_scope_uses_existing_component_relative_file_paths() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let repo = tempfile::tempdir().expect("repo dir");
        let run_git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .expect("git command");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "homeboy@example.com"]);
        run_git(&["config", "user.name", "Homeboy Test"]);
        let component_path = repo.path().join("packages/fixture");
        std::fs::create_dir_all(&component_path).expect("component dir");
        std::fs::write(component_path.join("initial.php"), "<?php\n").expect("initial source");
        run_git(&["add", "packages/fixture/initial.php"]);
        run_git(&["commit", "-q", "-m", "initial"]);
        run_git(&["tag", "fixture-v1.0.0"]);
        std::fs::write(component_path.join("changed.php"), "<?php echo 1;\n")
            .expect("changed source");
        std::fs::write(repo.path().join("outside.php"), "<?php echo 2;\n").expect("outside source");
        run_git(&["add", "packages/fixture/changed.php", "outside.php"]);
        run_git(&["commit", "-q", "-m", "change nested and outside files"]);

        let component = routed_lint_component(
            home.path(),
            &component_path,
            r#"#!/bin/sh
changed="$(cat "$HOMEBOY_LINT_CHANGED_FILES_FILE")"
test -f "$HOMEBOY_LINT_GLOB" || exit 2
printf '[{"tool":"fixture","message":"%s|%s","fingerprint":"scope","file":"changed.php"}]' "$HOMEBOY_LINT_GLOB" "$changed" > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
        );
        let run_dir = RunDir::create().expect("run dir");
        let mut args = lint_args();
        args.changed_since = Some("fixture-v1.0.0".to_string());
        args.json_summary = true;

        let workflow = run_main_lint_workflow(&component, &component_path, args, &run_dir)
            .expect("workflow result");

        let expected_glob = component_path.join("changed.php");
        assert!(expected_glob.is_file());
        assert_eq!(
            workflow.findings.as_ref().unwrap()[0].message,
            format!("{}|changed.php", expected_glob.display())
        );
        let manifest = run_dir.step_file(homeboy_core::engine::run_dir::files::LINT_CHANGED_FILES);
        assert!(manifest.is_file());
        assert_eq!(std::fs::read_to_string(manifest).unwrap(), "changed.php\n");
        assert!(!workflow.findings.as_ref().unwrap()[0]
            .message
            .contains("packages/fixture/packages/fixture"));
    });
}

#[test]
fn accepted_baseline_does_not_hide_later_route_producer_error() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let source = tempfile::tempdir().expect("source dir");
        let mut known = homeboy_core::finding::HomeboyFinding::builder("eslint", "known finding")
            .fingerprint("known")
            .build();
        known.location.file = Some("assets/app.js".to_string());
        crate::extension::lint::baseline::save_baseline(source.path(), "fixture", &[known])
            .expect("save baseline");
        let component = routed_lint_component(
            home.path(),
            source.path(),
            r#"#!/bin/sh
if [ "$HOMEBOY_STEP" = "php" ]; then
  printf '[]' > "$HOMEBOY_LINT_FINDINGS_FILE"
  exit 0
fi
printf '[{"tool":"eslint","message":"known finding","fingerprint":"known","file":"assets/app.js"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
printf '[{"tool":"eslint","status":"error","finding_count":1}]' > "$HOMEBOY_LINT_PRODUCERS_FILE"
exit 0
"#,
        );
        let run_dir = RunDir::create().expect("run dir");

        let workflow =
            run_main_lint_workflow(&component, source.path(), routed_lint_args(), &run_dir)
                .expect("workflow result");

        assert_eq!(workflow.status, "failed");
        assert_eq!(workflow.exit_code, 1);
        assert_eq!(workflow.findings.as_ref().map(Vec::len), Some(1));
        assert!(workflow
            .producer_summaries
            .iter()
            .any(|producer| producer.step.as_deref() == Some("js") && producer.status == "error"));
        assert!(workflow.baseline_comparison.is_none());
        assert!(workflow
            .baseline_provenance
            .as_ref()
            .is_some_and(|provenance| !provenance.compared));
    });
}

#[test]
fn lint_config_deserializes_changed_file_routes() {
    let config: homeboy_extension_contract::manifest_toolchain_config::LintConfig =
        serde_json::from_str(
            r#"{
                "extension_script": "scripts/lint.sh",
                "changed_file_routes": [
                    { "extensions": ["php"], "step": "phpcs,phpstan" },
                    { "globs": ["assets/**/*.css"], "step": "stylelint" }
                ]
            }"#,
        )
        .expect("parse lint config");

    assert_eq!(config.changed_file_routes.len(), 2);
    assert_eq!(config.changed_file_routes[0].extensions, vec!["php"]);
    assert_eq!(config.changed_file_routes[0].step, "phpcs,phpstan");
    assert_eq!(config.changed_file_routes[1].globs, vec!["assets/**/*.css"]);
    assert_eq!(config.changed_file_routes[1].step, "stylelint");
}

#[test]
fn scoped_lint_run_dir_disposes_success_and_failure_explicitly() {
    let _guard = homeboy_core::test_support::home_env_guard();
    let root = tempfile::tempdir().expect("runtime root");
    std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", root.path());

    let success = RunDir::create().expect("success run");
    let success_path = success.path().to_path_buf();
    finish_scoped_lint_run_dir(Some(&success), true);
    assert!(!success_path.exists());

    let failure = RunDir::create().expect("failure run");
    let failure_path = failure.path().to_path_buf();
    finish_scoped_lint_run_dir(Some(&failure), false);
    assert!(failure_path.exists());
    std::env::remove_var("HOMEBOY_RUNTIME_TMPDIR");
}
