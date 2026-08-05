use super::*;
use clap::Parser;

#[derive(Parser)]
struct TestCli {
    #[command(flatten)]
    bench: BenchArgs,
}

#[test]
fn parses_ci_profile_flag() {
    let cli = TestCli::try_parse_from(["bench", "homeboy", "--ci-profile", "perf"])
        .expect("bench should parse --ci-profile");

    assert_eq!(cli.bench.run.ci_profile.as_deref(), Some("perf"));
}

#[test]
fn parses_bench_list_rig_flag() {
    let cli = TestCli::try_parse_from(["bench", "list", "--rig", "studio-bfb"])
        .expect("bench list --rig should parse");

    match cli.bench.command.expect("list command") {
        BenchCommand::List(args) => assert_eq!(args.rig, vec!["studio-bfb".to_string()]),
        _ => panic!("expected bench list command"),
    }
}

#[test]
fn lab_workload_arguments_use_clap_values() {
    let cli = TestCli::try_parse_from(
        "bench --iterations 3 component --rig=a,b --rig c --extension=x --extension y"
            .split_whitespace(),
    )
    .expect("bench should parse");
    let workload = cli.bench.lab_rig_workload_arguments().unwrap();
    assert_eq!(workload.rig_ids, ["a", "b", "c"]);
    assert_eq!(workload.component.as_deref(), Some("component"));
    assert_eq!(workload.extension_overrides, ["x", "y"]);

    let cli = TestCli::try_parse_from("bench --run-id value --rig=r".split_whitespace()).unwrap();
    assert_eq!(
        cli.bench.lab_rig_workload_arguments().unwrap().component,
        None
    );
}

#[test]
fn parses_bench_list_json_flag() {
    let cli = TestCli::try_parse_from(["bench", "list", "--json"])
        .expect("bench list --json should parse");

    match cli.bench.command.expect("list command") {
        BenchCommand::List(args) => assert!(args.json),
        _ => panic!("expected bench list command"),
    }
}

#[test]
fn parses_repeated_scenario_flags() {
    let cli = TestCli::try_parse_from([
        "bench",
        "homeboy",
        "--scenario",
        "studio-agent-runtime",
        "--scenario",
        "wp-admin-load",
    ])
    .expect("bench --scenario should parse");

    assert_eq!(
        cli.bench.run.scenario_ids,
        vec![
            "studio-agent-runtime".to_string(),
            "wp-admin-load".to_string()
        ]
    );
}

#[test]
fn parses_profile_flag() {
    let cli = TestCli::try_parse_from(["bench", "--rig", "studio-bfb", "--profile", "smoke"])
        .expect("bench --profile should parse");

    assert_eq!(cli.bench.run.profile.as_deref(), Some("smoke"));
}

#[test]
fn scenario_and_profile_conflict() {
    let err = match TestCli::try_parse_from([
        "bench",
        "--rig",
        "studio-bfb",
        "--profile",
        "smoke",
        "--scenario",
        "boot",
    ]) {
        Ok(_) => panic!("--scenario and --profile should conflict"),
        Err(err) => err,
    };

    assert!(err.to_string().contains("cannot be used with"));
}

#[test]
fn parses_run_id_proof_label() {
    let cli = TestCli::try_parse_from(["bench", "homeboy", "--run-id", "proof-2026-06"])
        .expect("bench --run-id should parse");

    assert_eq!(cli.bench.run.run_id.as_deref(), Some("proof-2026-06"));
}

#[test]
fn non_integer_runs_hints_at_run_id() {
    let err = match TestCli::try_parse_from(["bench", "homeboy", "--runs", "proof-label"]) {
        Ok(_) => panic!("--runs with a non-integer should fail to parse"),
        Err(err) => err,
    };

    let message = err.to_string();
    assert!(
        message.contains("--runs is a numeric repetition count"),
        "expected repetition-count guidance, got: {message}"
    );
    assert!(
        message.contains("--run-id"),
        "expected pointer to --run-id for proof labels, got: {message}"
    );
}

#[test]
fn parses_dotted_setting_override_for_nested_bench_env() {
    let cli = TestCli::try_parse_from([
        "bench",
        "--rig",
        "woocommerce-performance",
        "--setting",
        "bench_env.WC_REST_BATCH_IMPORT_ITEMS=100",
    ])
    .expect("bench dotted --setting should parse");

    assert_eq!(
        cli.bench.run.setting_args.setting,
        vec![(
            "bench_env.WC_REST_BATCH_IMPORT_ITEMS".to_string(),
            "100".to_string()
        )]
    );
}

// --- Presentation vocabulary (#11138) -------------------------------------
//
// `bench --json` is the output-format sense of `--json`, now also spelled
// `--format json`. Both must keep working, and neither may fabricate JSON
// when the caller asked for something else.

#[test]
fn bare_bench_does_not_want_full_json() {
    let cli = TestCli::try_parse_from(["bench", "homeboy"]).expect("bench should parse");

    assert!(!cli.bench.wants_full_json());
}

#[test]
fn legacy_json_flag_still_wants_full_json() {
    let cli =
        TestCli::try_parse_from(["bench", "homeboy", "--json"]).expect("bench --json should parse");

    assert!(cli.bench.wants_full_json());
}

#[test]
fn format_json_wants_full_json_identically() {
    let cli = TestCli::try_parse_from(["bench", "homeboy", "--format", "json"])
        .expect("bench --format json should parse");

    assert!(cli.bench.wants_full_json());
}

#[test]
fn both_json_spellings_together_are_accepted() {
    let cli = TestCli::try_parse_from(["bench", "homeboy", "--json", "--format=json"])
        .expect("bench --json --format=json should parse");

    assert!(cli.bench.wants_full_json());
}

#[test]
fn non_json_formats_keep_the_compact_bench_summary() {
    for format in ["auto", "markdown", "text"] {
        let cli = TestCli::try_parse_from(["bench", "homeboy", "--format", format])
            .expect("bench --format should parse");

        assert!(
            !cli.bench.wants_full_json(),
            "--format {format} must not imply the full JSON payload"
        );
    }
}

/// `run` folds `--detail summary` onto the legacy field before dispatch, so
/// exercise the fold rather than the raw parse.
fn folded(args: &[&str]) -> BenchArgs {
    let mut bench = TestCli::try_parse_from(args)
        .expect("bench should parse")
        .bench;
    bench.apply_presentation_detail();
    bench
}

#[test]
fn detail_summary_is_the_canonical_json_summary_spelling() {
    assert!(!folded(&["bench", "homeboy"]).run.json_summary);
    assert!(
        folded(&["bench", "homeboy", "--json-summary"])
            .run
            .json_summary
    );
    assert!(
        folded(&["bench", "homeboy", "--detail", "summary"])
            .run
            .json_summary
    );
    assert!(
        !folded(&["bench", "homeboy", "--detail", "full"])
            .run
            .json_summary
    );
}

#[test]
fn both_detail_spellings_together_are_accepted() {
    assert!(
        folded(&["bench", "homeboy", "--json-summary", "--detail=summary"])
            .run
            .json_summary
    );
}

#[test]
fn detail_full_does_not_cancel_the_legacy_json_summary_flag() {
    // The legacy bool has no "off" state, so `--detail full` cannot be read
    // as a request to suppress a summary the caller explicitly asked for.
    assert!(
        folded(&["bench", "homeboy", "--json-summary", "--detail=full"])
            .run
            .json_summary
    );
}

#[test]
fn detail_is_independent_of_format() {
    let bench = folded(&["bench", "homeboy", "--detail=summary"]);

    assert!(bench.run.json_summary);
    assert!(
        !bench.wants_full_json(),
        "--detail must not imply the full JSON payload"
    );

    let json_only = folded(&["bench", "homeboy", "--format=json"]);
    assert!(json_only.wants_full_json());
    assert!(
        !json_only.run.json_summary,
        "--format must not imply the compact summary"
    );
}

#[test]
fn presentation_flags_do_not_disturb_the_bench_run_group() {
    let bench = folded(&[
        "bench",
        "homeboy",
        "--format=json",
        "--detail=summary",
        "--iterations",
        "3",
    ]);

    assert!(bench.wants_full_json());
    assert!(bench.run.json_summary);
    assert_eq!(bench.run.iterations, 3);
}

#[test]
fn explicit_passthrough_preserves_a_runner_owned_format_flag() {
    // Declaring `--format` on bench must not let `filter_passthrough_args`
    // swallow a bench runner's own `--format`. `normalize` marks the explicit
    // `--` boundary with a sentinel, and that is what protects it.
    let normalized = crate::commands::utils::args::normalize(
        ["homeboy", "bench", "homeboy", "--", "--format", "terse"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<String>>(),
    );

    // Everything clap hands to the `last = true` passthrough field: the
    // sentinel `normalize` inserted, then the runner's own arguments.
    let passthrough: Vec<String> = normalized
        .iter()
        .skip_while(|arg| arg.as_str() != "--")
        .skip(1)
        .cloned()
        .collect();

    assert_eq!(
        filter_homeboy_flags(&passthrough),
        vec!["--format".to_string(), "terse".to_string()]
    );
}
