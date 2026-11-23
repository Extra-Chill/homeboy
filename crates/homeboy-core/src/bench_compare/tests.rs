use super::*;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn route(
    name: &str,
    content: &str,
    blockers: &[&str],
    findings: &[&str],
) -> BenchCompareRouteObservation {
    BenchCompareRouteObservation {
        route: name.to_string(),
        content: Some(content.to_string()),
        blockers: blockers
            .iter()
            .map(|b| BenchCompareFinding::Text(b.to_string()))
            .collect(),
        findings: findings
            .iter()
            .map(|f| BenchCompareFinding::Text(f.to_string()))
            .collect(),
        ..Default::default()
    }
}

fn side(routes: Vec<BenchCompareRouteObservation>) -> BenchCompareSideResults {
    BenchCompareSideResults {
        schema: Some(SIDE_RESULTS_SCHEMA.to_string()),
        routes,
    }
}

#[test]
fn diff_classifies_routes_and_summarizes() {
    let base = side(vec![
        route("/fixed", "a", &["missing-block"], &[]),
        route("/broken", "a", &[], &[]),
        route("/same", "a", &[], &["f1"]),
        route("/gone", "a", &[], &[]),
    ]);
    let head = side(vec![
        route("/fixed", "b", &[], &[]),
        route("/broken", "a", &["invalid-markup"], &[]),
        route("/same", "a", &[], &["f1"]),
        route("/new", "a", &[], &[]),
    ]);

    let routes = diff_routes(&base, &head);
    let status = |name: &str| {
        routes
            .iter()
            .find(|r| r.route == name)
            .map(|r| r.status)
            .unwrap()
    };
    assert_eq!(status("/fixed"), BenchCompareRouteStatus::Improved);
    assert_eq!(status("/broken"), BenchCompareRouteStatus::Regressed);
    assert_eq!(status("/same"), BenchCompareRouteStatus::Unchanged);
    assert_eq!(status("/gone"), BenchCompareRouteStatus::MissingOnHead);
    assert_eq!(status("/new"), BenchCompareRouteStatus::AddedOnHead);

    let fixed = routes.iter().find(|r| r.route == "/fixed").unwrap();
    assert!(fixed.output_changed && fixed.content.changed);
    assert_eq!(fixed.blockers.resolved, vec!["missing-block".to_string()]);

    let summary = summarize_routes(&routes);
    assert_eq!(summary.routes, 5);
    assert_eq!(summary.improved, 1);
    assert_eq!(summary.regressed, 1);
    assert_eq!(summary.unchanged, 1);
    assert_eq!(summary.new_blockers, 1);
    assert_eq!(summary.resolved_blockers, 1);
    assert_eq!(summary.missing_on_head, 1);
}

#[test]
fn findings_decide_only_when_blockers_are_net_neutral() {
    let base = side(vec![route("/r", "a", &["b1"], &["f1", "f2"])]);
    let head = side(vec![route("/r", "a", &["b1"], &["f1"])]);
    assert_eq!(
        diff_routes(&base, &head)[0].status,
        BenchCompareRouteStatus::Improved
    );
}

#[test]
fn findings_accept_strings_and_objects() {
    let results: BenchCompareSideResults = serde_json::from_str(
        r#"{"routes":[{"route":"/a","blockers":["x",{"id":"y","message":"why"},{"message":"z"}]}]}"#,
    )
    .unwrap();
    let keys: Vec<String> = results.routes[0]
        .blockers
        .iter()
        .map(BenchCompareFinding::key)
        .collect();
    assert_eq!(keys, ["x", "y", "z"]);
}

#[test]
fn precomputed_digests_compare_without_raw_content() {
    let mut base = route("/r", "", &[], &[]);
    base.content = None;
    base.css_sha256 = Some("ABC".to_string());
    let mut head = base.clone();
    head.css_sha256 = Some("abc".to_string());
    let delta = &diff_routes(&side(vec![base]), &side(vec![head]))[0];
    assert!(!delta.css.changed);
}

#[test]
fn policy_fails_on_new_blockers_and_always_on_leftovers() {
    let passed_side = BenchCompareSideReport {
        passed: true,
        ..Default::default()
    };
    let lifecycle = BenchCompareLifecycle::default();
    let summary = BenchCompareSummary {
        new_blockers: 1,
        ..Default::default()
    };
    assert!(
        !evaluate_policy(
            BenchCompareFailOn::NewBlockers,
            &summary,
            &passed_side,
            &passed_side,
            &lifecycle
        )
        .passed
    );
    assert!(
        evaluate_policy(
            BenchCompareFailOn::Never,
            &summary,
            &passed_side,
            &passed_side,
            &lifecycle
        )
        .passed
    );
    let leaked = BenchCompareLifecycle {
        leftover_process_scopes: 1,
        ..Default::default()
    };
    assert!(
        !evaluate_policy(
            BenchCompareFailOn::Never,
            &BenchCompareSummary::default(),
            &passed_side,
            &passed_side,
            &leaked
        )
        .passed
    );
}

#[test]
fn spec_rejects_unknown_fields_and_parses_policy() {
    let spec = BenchCompareSpec::parse(
        r#"{"component":"blocks-engine","corpus":"$CAPTURE","fail_on":"regressions"}"#,
    )
    .unwrap();
    assert_eq!(spec.fail_on, Some(BenchCompareFailOn::Regressions));
    assert!(BenchCompareSpec::parse(r#"{"corpsu":"x"}"#).is_err());
}

#[test]
fn side_results_fail_closed_on_empty_duplicate_or_missing_results() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("results.json");
    assert!(super::execute::load_side_results(&path).is_err());
    for json in [
        r#"{}"#,
        r#"{"routes":[]}"#,
        r#"{"routes":[{"route":"/"},{"route":"/"}]}"#,
    ] {
        std::fs::write(&path, json).unwrap();
        assert!(super::execute::load_side_results(&path).is_err(), "{json}");
    }
}

#[test]
fn failed_or_empty_side_cannot_report_no_change_even_with_never_policy() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "--quiet"]);
    git(
        repo.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "base"],
    );
    let corpus = tempfile::tempdir().unwrap();
    let stop = AtomicBool::new(false);
    for runner in [
        "exit 42",
        "printf '{}' > \"$HOMEBOY_BENCH_COMPARE_RESULTS\"",
        "true",
    ] {
        let artifacts = tempfile::tempdir().unwrap();
        let output = run_bench_compare(&BenchCompareRequest {
            compare_id: "failed".into(),
            component_id: "fixture".into(),
            repo_root: repo.path().to_path_buf(),
            component_relative: Default::default(),
            base: "HEAD".into(),
            head: "HEAD".into(),
            corpus: corpus.path().to_path_buf(),
            artifact_dir: artifacts.path().to_path_buf(),
            runner: vec![runner.into()],
            fail_on: BenchCompareFailOn::Never,
            side_timeout: Duration::from_secs(20),
            stop: &stop,
        })
        .unwrap();
        assert_eq!(output.status, "failed");
        assert!(!output.policy.passed);
        assert!(output.base.error.is_some());
        assert!(output.head.error.is_some());
        assert_eq!(output.lifecycle.leftover_process_scopes, 0);
    }
}

#[test]
fn worktree_head_is_an_isolated_dirty_snapshot() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "--quiet"]);
    std::fs::write(repo.path().join("tracked"), "base").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "--quiet", "-m", "base"]);
    std::fs::write(repo.path().join("tracked"), "dirty").unwrap();
    std::fs::write(repo.path().join("untracked"), "new").unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    let stop = AtomicBool::new(false);
    let output = run_bench_compare(&BenchCompareRequest {
        compare_id: "snapshot".into(), component_id: "fixture".into(),
        repo_root: repo.path().to_path_buf(), component_relative: Default::default(),
        base: "HEAD".into(), head: WORKTREE_HEAD.into(),
        corpus: corpus.path().to_path_buf(), artifact_dir: artifacts.path().to_path_buf(),
        runner: vec![r#"if [ "$HOMEBOY_BENCH_COMPARE_SIDE" = head ]; then test "$(cat tracked)" = dirty && test "$(cat untracked)" = new || exit 1; fi; printf '{"routes":[{"route":"/"}]}' > "$HOMEBOY_BENCH_COMPARE_RESULTS"; echo mutated > tracked"#.into()],
        fail_on: BenchCompareFailOn::Never, side_timeout: Duration::from_secs(20), stop: &stop,
    }).unwrap();
    assert!(output.policy.passed, "{output:?}");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked")).unwrap(),
        "dirty"
    );
    assert_eq!(output.lifecycle.worktrees_removed, 2);
}

#[test]
fn concurrent_materialization_has_private_ref_namespaces() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "--quiet"]);
    git(
        repo.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "base"],
    );
    std::thread::scope(|scope| {
        for side in ["one", "two"] {
            let root = repo.path();
            scope.spawn(move || {
                let checkout =
                    super::execute::MaterializedWorktree::create(root, "HEAD", side).unwrap();
                git(
                    &checkout.path,
                    &["update-ref", "refs/heads/private-run", "HEAD"],
                );
                assert!(checkout.remove());
            });
        }
    });
    let refs = Command::new("git")
        .args(["show-ref", "--verify", "refs/heads/private-run"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(!refs.status.success());
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.test")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.test")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// End to end: both sides run in their own revision, a backgrounded
/// long-lived process (standing in for `php -S`) is torn down, the worktree is
/// removed, and the artifact asserts zero leftover process scopes.
#[cfg(unix)]
#[test]
fn compare_runs_both_sides_and_leaves_no_processes_or_worktrees() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "--quiet"]);
    std::fs::write(root.join("blocker.txt"), "missing-block\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "base"]);
    std::fs::write(root.join("blocker.txt"), "").unwrap();
    git(root, &["commit", "--quiet", "-am", "head"]);

    let corpus = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    // Leave a server-like process behind, then report the route.
    let runner = r#"sleep 300 & b=$(cat blocker.txt); if [ -n "$b" ]; then bl="[\"$b\"]"; else bl="[]"; fi; printf '{"routes":[{"route":"/home","content":"%s","blockers":%s}]}' "$HOMEBOY_BENCH_COMPARE_SIDE" "$bl" > "$HOMEBOY_BENCH_COMPARE_RESULTS""#;
    let stop = AtomicBool::new(false);
    let output = run_bench_compare(&BenchCompareRequest {
        compare_id: "compare-test".to_string(),
        component_id: "fixture".to_string(),
        repo_root: root.to_path_buf(),
        component_relative: Default::default(),
        base: "HEAD~1".to_string(),
        head: "HEAD".to_string(),
        corpus: corpus.path().to_path_buf(),
        runner: vec![runner.to_string()],
        fail_on: BenchCompareFailOn::NewBlockers,
        side_timeout: Duration::from_secs(60),
        artifact_dir: artifacts.path().to_path_buf(),
        stop: &stop,
    })
    .unwrap();

    assert!(output.base.passed, "{:?}", output.base);
    assert!(output.head.passed, "{:?}", output.head);
    assert_eq!(output.summary.improved, 1);
    assert_eq!(output.summary.resolved_blockers, 1);
    assert_eq!(output.lifecycle.leftover_process_scopes, 0);
    assert_eq!(output.lifecycle.worktrees_materialized, 2);
    assert_eq!(output.lifecycle.worktrees_removed, 2);
    assert!(output.policy.passed, "{:?}", output.policy);
    for step in output.base.steps.iter().chain(&output.head.steps) {
        assert!(!step.leftover_processes);
        if let Some(pgid) = step.process_group_id {
            assert!(!homeboy_engine_primitives::command::process_group_is_running(pgid).unwrap());
        }
    }
    assert!(Path::new(&output.artifact_path).is_file());

    let worktrees = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(root)
        .output()
        .unwrap();
    let listed = String::from_utf8_lossy(&worktrees.stdout);
    assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");
}
