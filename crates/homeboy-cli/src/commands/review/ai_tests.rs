use super::*;
use clap::Parser;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    review: ReviewArgs,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn fixture(home: &Path, mode: &str) -> (std::path::PathBuf, String, String) {
    let repo = home.join("candidate");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("source.txt"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("source.txt"), "head\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "candidate"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let extension = home.join(".config/homeboy/extensions/fixture-review");
    fs::create_dir_all(&extension).unwrap();
    fs::write(extension.join("fixture-review.json"), serde_json::to_vec(&serde_json::json!({
        "name":"Fixture reviewer", "version":"1.0.0", "settings":[{"id":"mode","type":"string","label":"Mode","default":mode}],
        "contract_producers":[{"id":"review", "phase":"result", "invocation":{
            "script":"review.py", "env":["PATH"],
            "input_schema":review::ai::REQUEST_SCHEMA, "output_schema":review::ai::RESPONSE_SCHEMA
        },"produces":[{"kind":"evidence","schema":review::ai::RESPONSE_SCHEMA}]}]
    })).unwrap()).unwrap();
    fs::write(extension.join("review.py"), r#"#!/usr/bin/env python3
import json, sys, time
r=json.load(sys.stdin)
mode=r['settings']['mode']
if mode=='timeout': time.sleep(5)
if mode=='overflow':
    sys.stdout.write('x'*5000000); sys.stdout.flush(); time.sleep(5)
if mode=='invalid': print('invalid'); sys.exit(0)
if mode=='error': print('provider failure',file=sys.stderr); sys.exit(7)
o={'schema':'homeboy/ai-review-response/v1','execution':'skipped' if mode=='skip' else 'completed',
   'verdict':'block','base_sha':r['base_sha'],'head_sha':'bad' if mode=='stale' else r['head_sha'],
   'findings':[{'tool':'fixture','severity':'warning','message':'Validate this caller','location':{'file':'source.txt','line':1}}],
   'reason':'diff cap' if mode=='skip' else None,'provenance':{'model':'fixture'},'usage':{'cost_usd':0.12}}
print(json.dumps(o))
"#).unwrap();
    fs::set_permissions(
        extension.join("review.py"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    (repo, base, head)
}

fn invoke(repo: &Path, base: &str, extra: &[&str]) -> CmdResult<Value> {
    let mut argv = vec![
        "review",
        "ai",
        "--path",
        repo.to_str().unwrap(),
        "--extension",
        "fixture-review",
        "--changed-since",
        base,
    ];
    argv.extend(extra);
    let mut args = Cli::try_parse_from(argv).unwrap().review;
    args.project_effective_child_args().unwrap();
    run(args)
}

#[test]
fn ai_cli_dispatches_private_local_producer_and_preserves_candidate_findings() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let (repo, base, head) = fixture(home.path(), "findings");
        let (output, code) = invoke(&repo, &base, &[]).unwrap();
        assert_eq!(code, 0, "model block is advisory");
        assert_eq!(output["ai"]["execution"], "completed");
        assert_eq!(output["ai"]["verdict"], "block");
        assert_eq!(output["ai"]["request"]["base_sha"], base);
        assert_eq!(output["ai"]["request"]["head_sha"], head);
        assert_eq!(
            output["artifact"]["commands"][0]["findings"][0]["message"],
            "Validate this caller"
        );
        assert_eq!(output["ai"]["usage"]["cost_usd"], 0.12);
        let run_id = output["observation"]["run_id"].as_str().unwrap();
        assert!(output["observation"].is_object());
        // The generic observation store retains candidate-bound raw evidence.
        let store = ObservationStore::open_initialized().unwrap();
        let run = store.get_run(run_id).unwrap().unwrap();
        assert_eq!(run.metadata_json["ai"]["request"]["head_sha"], head);
        let rendered = review::render::render_ai_review(
            &serde_json::from_value(output["ai"].clone()).unwrap(),
        );
        assert!(rendered.contains("advisory"));
        assert!(rendered.contains("Validate this caller"));
    });
}

#[test]
fn ai_cli_does_not_accept_exit_zero_skips_invalid_results_or_stale_identity() {
    for mode in ["skip", "invalid", "stale", "error"] {
        homeboy_core::test_support::with_isolated_home(|home| {
            let (repo, base, _) = fixture(home.path(), mode);
            let (output, code) = invoke(&repo, &base, &[]).unwrap();
            assert_eq!(code, 2, "{mode}");
            assert_ne!(output["ai"]["execution"], "completed", "{mode}");
            assert!(output["ai"]["reason"].is_string());
        });
    }
}

#[test]
fn ai_cli_bounds_timeout_and_output_and_rejects_dirty_context() {
    for mode in ["timeout", "overflow"] {
        homeboy_core::test_support::with_isolated_home(|home| {
            let (repo, base, _) = fixture(home.path(), mode);
            let start = Instant::now();
            let (output, code) = invoke(&repo, &base, &["--ai-timeout-seconds", "1"]).unwrap();
            assert_eq!(code, 2);
            assert_eq!(output["ai"]["execution"], "failed");
            assert!(start.elapsed() < Duration::from_secs(5));
            fs::write(repo.join("source.txt"), "dirty context").unwrap();
            assert!(invoke(&repo, &base, &[]).is_err());
        });
    }
}

#[test]
fn ai_umbrella_attachment_keeps_deterministic_failures_and_renders_advisory_findings() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let (repo, base, _) = fixture(home.path(), "findings");
        let (value, _) = invoke(&repo, &base, &[]).unwrap();
        let result: review::ai::AiReviewResult =
            serde_json::from_value(value["ai"].clone()).unwrap();
        let input = ReviewOutputInput {
            component: "fixture".to_string(),
            plan: build_quality_plan(QualityPlanOptions::review("fixture")),
            observation: None,
            scope: "changed-since".to_string(),
            changed_since: Some(base),
            changed_file_count: Some(1),
            head_ref: result.request.head_sha.clone(),
            hints: Vec::new(),
        };
        let mut output =
            ReviewService::skipped_output(input, "fixture deterministic stages", false);
        output.summary.passed = false;
        output.summary.status = "failed".to_string();
        assert_eq!(result.attach(&mut output), 0);
        assert!(
            !output.summary.passed,
            "AI verdict cannot override deterministic failure"
        );
        assert_eq!(output.summary.total_findings, 1);
        assert!(review::render::render_pr_comment(&output).contains("Validate this caller"));
        let mut skipped = output.ai.take().unwrap();
        skipped.execution = review::ai::AiReviewExecution::Skipped;
        skipped.reason = Some("diff cap reached".to_string());
        assert_eq!(skipped.attach(&mut output), 2);
        assert_eq!(output.summary.status, "incomplete");
        assert!(output
            .summary
            .hints
            .iter()
            .any(|hint| hint.contains("diff cap")));
    });
}
