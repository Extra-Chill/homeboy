use super::*;
use clap::Parser;

#[derive(Parser)]
struct TestCli {
    #[command(flatten)]
    bench: BenchArgs,
}

#[test]
fn bench_compare_stays_local() {
    let cli = TestCli::try_parse_from([
        "bench",
        "compare",
        "--component",
        "blocks-engine",
        "--base",
        "abc123",
        "--head",
        "def456",
    ])
    .expect("bench compare should parse");

    assert!(!cli.bench.is_lab_offload_command());
    assert!(cli.bench.lab_rig_workload_arguments().is_none());
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
fn detail_folds_onto_json_summary_independently_of_format() {
    // (argv, expected json_summary, expected wants_full_json)
    let cases: &[(&[&str], bool, bool)] = &[
        (&["bench", "homeboy"], false, false),
        (&["bench", "homeboy", "--json-summary"], true, false),
        (&["bench", "homeboy", "--detail", "summary"], true, false),
        (&["bench", "homeboy", "--detail", "full"], false, false),
        // `--json-summary` has no "off" state, so `--detail full` cannot cancel it.
        (
            &["bench", "homeboy", "--json-summary", "--detail=full"],
            true,
            false,
        ),
        // `--format` must not imply the compact summary.
        (&["bench", "homeboy", "--format=json"], false, true),
    ];
    for (argv, json_summary, full_json) in cases {
        let bench = folded(argv);
        assert_eq!(bench.run.json_summary, *json_summary, "argv: {argv:?}");
        assert_eq!(bench.wants_full_json(), *full_json, "argv: {argv:?}");
    }
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
