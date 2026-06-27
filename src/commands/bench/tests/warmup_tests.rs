use super::*;

#[test]
fn parses_warmup_flag() {
    let cli = TestCli::try_parse_from(["bench", "homeboy", "--warmup", "3"])
        .expect("bench --warmup should parse");

    assert_eq!(cli.bench.run.warmup, Some(3));
}

#[test]
fn rejects_negative_warmup_flag() {
    let err = match TestCli::try_parse_from(["bench", "homeboy", "--warmup", "-1"]) {
        Ok(_) => panic!("negative warmup must fail at parse time"),
        Err(err) => err,
    };

    assert!(
        err.to_string().contains("invalid value"),
        "expected invalid value error, got: {}",
        err
    );
}

#[test]
fn filter_strips_warmup_forms() {
    let args = vec![
        "--warmup".to_string(),
        "3".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);

    let args = vec!["--warmup=3".to_string(), "--keep".to_string()];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn unrigged_bench_forwards_cli_warmup() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let mut args = run_args(Some("studio"), Vec::new(), Vec::new());
        args.run.warmup = Some(4);

        let (output, exit_code) = run(args, &GlobalArgs {}).expect("bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(first_warmup_metric(output), 4.0);
    });
}

#[test]
fn unrigged_bench_omits_warmup_env_by_default() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(Some("studio"), Vec::new(), Vec::new()),
            &GlobalArgs {},
        )
        .expect("bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(first_warmup_metric(output), -1.0);
    });
}

#[test]
fn single_rig_bench_forwards_rig_warmup() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "rig-a", "studio", component_dir.path());
        set_rig_warmup(home, "rig-a", 6);

        let (output, exit_code) = run(
            run_args(None, vec!["rig-a".to_string()], Vec::new()),
            &GlobalArgs {},
        )
        .expect("rig bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(first_warmup_metric(output), 6.0);
    });
}

#[test]
fn cli_warmup_overrides_rig_warmup() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "rig-a", "studio", component_dir.path());
        set_rig_warmup(home, "rig-a", 6);

        let mut args = run_args(None, vec!["rig-a".to_string()], Vec::new());
        args.run.warmup = Some(2);

        let (output, exit_code) = run(args, &GlobalArgs {}).expect("rig bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(first_warmup_metric(output), 2.0);
    });
}

#[test]
fn cross_rig_bench_uses_each_rig_warmup() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());
        set_rig_warmup(home, "rig-a", 2);
        set_rig_warmup(home, "rig-b", 5);

        let (output, exit_code) = run(
            run_args(
                None,
                vec!["rig-a".to_string(), "rig-b".to_string()],
                Vec::new(),
            ),
            &GlobalArgs {},
        )
        .expect("cross-rig bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                assert_eq!(
                    result.rigs[0]
                        .results
                        .as_ref()
                        .expect("rig-a results")
                        .scenarios[0]
                        .metrics
                        .get("warmup_iterations"),
                    Some(2.0)
                );
                assert_eq!(
                    result.rigs[1]
                        .results
                        .as_ref()
                        .expect("rig-b results")
                        .scenarios[0]
                        .metrics
                        .get("warmup_iterations"),
                    Some(5.0)
                );
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn cross_rig_cli_warmup_overrides_all_rigs() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());
        set_rig_warmup(home, "rig-a", 2);
        set_rig_warmup(home, "rig-b", 5);

        let mut args = run_args(
            None,
            vec!["rig-a".to_string(), "rig-b".to_string()],
            Vec::new(),
        );
        args.run.warmup = Some(9);

        let (output, exit_code) = run(args, &GlobalArgs {}).expect("cross-rig bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                for rig in result.rigs {
                    assert_eq!(
                        rig.results.expect("rig results").scenarios[0]
                            .metrics
                            .get("warmup_iterations"),
                        Some(9.0)
                    );
                }
            }
            _ => panic!("expected comparison output"),
        }
    });
}
