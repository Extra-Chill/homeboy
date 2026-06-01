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
