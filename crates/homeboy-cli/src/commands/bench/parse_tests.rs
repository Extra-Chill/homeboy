use super::*;
use clap::Parser;

#[derive(Parser)]
struct TestCli {
    #[command(flatten)]
    bench: BenchArgs,
}

#[test]
fn parses_bench_compare_and_keeps_it_local() {
    let cli = TestCli::try_parse_from([
        "bench",
        "compare",
        "--component",
        "blocks-engine",
        "--base",
        "abc123",
        "--head",
        "def456",
        "--corpus",
        "/captures",
    ])
    .expect("bench compare should parse");

    assert!(!cli.bench.is_lab_offload_command());
    assert!(cli.bench.lab_rig_workload_arguments().is_none());
    match cli.bench.command.expect("compare command") {
        BenchCommand::Compare(args) => {
            assert_eq!(args.component.as_deref(), Some("blocks-engine"));
            assert_eq!(args.base.as_deref(), Some("abc123"));
            assert_eq!(args.head.as_deref(), Some("def456"));
            assert_eq!(args.corpus.as_deref(), Some("/captures"));
        }
        _ => panic!("expected bench compare command"),
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
// `bench --json` and `--format json` both request the full JSON payload;
// neither may fabricate JSON when the caller asked for something else.

#[test]
fn only_json_format_spellings_want_the_full_json_payload() {
    let cases: &[(&[&str], bool)] = &[
        (&[], false),
        (&["--json"], true),
        (&["--format", "json"], true),
        (&["--json", "--format=json"], true),
        (&["--format", "auto"], false),
        (&["--format", "markdown"], false),
        (&["--format", "text"], false),
    ];
    for (flags, expected) in cases {
        let argv: Vec<&str> = ["bench", "homeboy"]
            .into_iter()
            .chain(flags.iter().copied())
            .collect();
        let cli = TestCli::try_parse_from(argv).expect("bench should parse");
        assert_eq!(cli.bench.wants_full_json(), *expected, "flags: {flags:?}");
    }
}

/// `run` folds `--detail summary` onto `--json-summary` before dispatch, so
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
fn detail_full_does_not_cancel_the_json_summary_flag() {
    // The `--json-summary` bool has no "off" state, so `--detail full` cannot be read
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
