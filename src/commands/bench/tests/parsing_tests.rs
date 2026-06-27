use super::*;

#[test]
fn filter_strips_boolean_flags() {
    let args = vec!["--ratchet".to_string(), "--filter=Scenario".to_string()];
    let result = filter_homeboy_flags(&args);
    assert_eq!(result, vec!["--filter=Scenario"]);
}

#[test]
fn parses_force_hot_after_bench_without_losing_settings() {
    let normalized = crate::commands::utils::args::normalize(
        [
            "homeboy",
            "bench",
            "--rig",
            "studio-bfb",
            "--iterations",
            "1",
            "--force-hot",
            "--setting",
            "studio_site_build_prompt_variant=astro-docs-content-collection",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    );

    let cli = crate::cli_surface::Cli::try_parse_from(normalized)
        .expect("bench --force-hot --setting should parse");

    assert!(cli.force_hot);
    let crate::cli_surface::Commands::Bench(args) = cli.command else {
        panic!("expected bench command");
    };
    assert_eq!(
        args.run.setting_args.setting,
        vec![(
            "studio_site_build_prompt_variant".to_string(),
            "astro-docs-content-collection".to_string()
        )]
    );
    assert!(
        !args.run.args.iter().any(|arg| arg == "--force-hot"),
        "--force-hot must not be forwarded to bench workloads"
    );
}

#[test]
fn parses_rig_run_options_without_component() {
    let cli = TestCli::try_parse_from([
        "bench",
        "--rig",
        "studio-agent-sdk,studio-agent-pi",
        "--scenario",
        "studio-agent-runtime",
        "--runs",
        "3",
        "--iterations",
        "1",
    ])
    .expect("rig bench options without component should parse");

    assert_eq!(
        cli.bench.run.rig,
        vec!["studio-agent-sdk", "studio-agent-pi"]
    );
    assert_eq!(cli.bench.run.scenario_ids, vec!["studio-agent-runtime"]);
    assert_eq!(cli.bench.run.runs, 3);
    assert_eq!(cli.bench.run.iterations, 1);
    assert!(cli.bench.run.comp.id().is_none());
}

#[test]
fn parses_rig_order_flag() {
    let cli = TestCli::try_parse_from([
        "bench",
        "--rig",
        "studio-agent-sdk,studio-agent-pi",
        "--rig-order",
        "reverse",
    ])
    .expect("bench --rig-order should parse");

    assert_eq!(cli.bench.run.rig_order, BenchRigOrder::Reverse);
}

#[test]
fn parses_rig_concurrency_flag() {
    let cli = TestCli::try_parse_from([
        "bench",
        "--rig",
        "studio-agent-sdk,studio-agent-pi",
        "--rig-concurrency",
        "2",
    ])
    .expect("bench --rig-concurrency should parse");

    assert_eq!(cli.bench.run.rig_concurrency, 2);
}

#[test]
fn rejects_allow_stale_dependencies_flag() {
    let err = match TestCli::try_parse_from(["bench", "homeboy", "--allow-stale-dependencies"]) {
        Ok(_) => panic!("stale dependency bypass should not parse"),
        Err(err) => err,
    };

    assert_eq!(err.kind(), ErrorKind::UnknownArgument);
}

#[test]
fn filter_strips_all_boolean_flags() {
    let args = vec![
        "--baseline".to_string(),
        "--ignore-baseline".to_string(),
        "--ratchet".to_string(),
        "--json-summary".to_string(),
    ];
    assert!(filter_homeboy_flags(&args).is_empty());
}

#[test]
fn filter_strips_iterations_space_form() {
    let args = vec![
        "--iterations".to_string(),
        "50".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);
}

#[test]
fn filter_strips_iterations_equals_form() {
    let args = vec!["--iterations=50".to_string(), "--keep".to_string()];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn parses_side_by_side_report_flag() {
    let cli = TestCli::try_parse_from([
        "bench",
        "studio",
        "--rig",
        "baseline,candidate",
        "--report",
        "side-by-side",
    ])
    .expect("bench --report side-by-side should parse");

    assert_eq!(cli.bench.run.report, vec![BenchReportFormat::SideBySide]);
}

#[test]
fn parses_shared_state_and_concurrency_flags() {
    let cli = TestCli::try_parse_from([
        "bench",
        "homeboy",
        "--shared-state",
        "/tmp/foo",
        "--concurrency",
        "4",
    ])
    .expect("shared-state and concurrency flags should parse");

    assert_eq!(cli.bench.run.shared_state, Some(PathBuf::from("/tmp/foo")));
    assert_eq!(cli.bench.run.concurrency, 4);
}

#[test]
fn filter_strips_shared_state_and_concurrency_forms() {
    let args = vec![
        "--shared-state".to_string(),
        "/tmp/foo".to_string(),
        "--concurrency".to_string(),
        "4".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);

    let args = vec![
        "--shared-state=/tmp/foo".to_string(),
        "--concurrency=4".to_string(),
        "--keep".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn filter_strips_rig_order_forms() {
    let args = vec![
        "--rig-order".to_string(),
        "reverse".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);

    let args = vec!["--rig-order=reverse".to_string(), "--keep".to_string()];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn filter_strips_report_forms() {
    let args = vec![
        "--report".to_string(),
        "side-by-side".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);

    let args = vec!["--report=side-by-side".to_string(), "--keep".to_string()];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn filter_strips_rig_concurrency_forms() {
    let args = vec![
        "--rig-concurrency".to_string(),
        "2".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);

    let args = vec!["--rig-concurrency=2".to_string(), "--keep".to_string()];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn filter_strips_regression_threshold_forms() {
    let args = vec![
        "--regression-threshold".to_string(),
        "10".to_string(),
        "--keep".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);

    let args = vec![
        "--regression-threshold=10".to_string(),
        "--keep".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn filter_preserves_unknown_flags() {
    let args = vec![
        "--filter=Scenario".to_string(),
        "--verbose".to_string(),
        "extra".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), args);
}

#[test]
fn filter_handles_empty() {
    assert!(filter_homeboy_flags(&[]).is_empty());
}

#[test]
fn filter_handles_mixed() {
    let args = vec![
        "--ratchet".to_string(),
        "--iterations".to_string(),
        "25".to_string(),
        "--filter=hot_path".to_string(),
        "--regression-threshold=7.5".to_string(),
        "--verbose".to_string(),
    ];
    assert_eq!(
        filter_homeboy_flags(&args),
        vec!["--filter=hot_path", "--verbose"]
    );
}
