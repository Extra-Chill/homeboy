use super::*;

fn warmup_metrics(output: BenchOutput) -> Vec<f64> {
    match output {
        BenchOutput::Comparison(result) => result
            .rigs
            .into_iter()
            .map(|rig| {
                rig.results.expect("rig results").scenarios[0]
                    .metrics
                    .get("warmup_iterations")
                    .expect("warmup metric")
            })
            .collect(),
        _ => panic!("expected comparison output"),
    }
}

#[test]
fn unrigged_bench_forwards_cli_warmup_and_omits_it_by_default() {
    for (cli_warmup, expected) in [(None, -1.0), (Some(4), 4.0)] {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_registered_component(home, "studio", component_dir.path());

            let mut args = run_args(Some("studio"), Vec::new(), Vec::new());
            args.run.warmup = cli_warmup;

            let (output, exit_code) = run(args).expect("bench should run");

            assert_eq!(exit_code, 0);
            assert_eq!(first_warmup_metric(output), expected, "{cli_warmup:?}");
        });
    }
}

#[test]
fn single_rig_bench_uses_rig_warmup_unless_cli_overrides() {
    for (cli_warmup, expected) in [(None, 6.0), (Some(2), 2.0)] {
        with_isolated_home(|home| {
            write_bench_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            write_rig(home, "rig-a", "studio", component_dir.path());
            set_rig_warmup(home, "rig-a", 6);

            let mut args = run_args(None, vec!["rig-a".to_string()], Vec::new());
            args.run.warmup = cli_warmup;

            let (output, exit_code) = run(args).expect("rig bench should run");

            assert_eq!(exit_code, 0);
            assert_eq!(first_warmup_metric(output), expected, "{cli_warmup:?}");
        });
    }
}

#[test]
fn cross_rig_bench_uses_each_rig_warmup_unless_cli_overrides() {
    for (cli_warmup, expected) in [(None, vec![2.0, 5.0]), (Some(9), vec![9.0, 9.0])] {
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
            args.run.warmup = cli_warmup;

            let (output, exit_code) = run(args).expect("cross-rig bench should run");

            assert_eq!(exit_code, 0);
            assert_eq!(warmup_metrics(output), expected, "{cli_warmup:?}");
        });
    }
}
