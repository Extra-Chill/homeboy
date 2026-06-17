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
