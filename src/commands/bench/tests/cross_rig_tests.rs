use super::*;

#[test]
fn cross_rig_run_passes_selector_to_each_rig() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());

        let (output, exit_code) = run(
            run_args(
                None,
                vec!["rig-a".to_string(), "rig-b".to_string()],
                vec!["rig-slow".to_string()],
            ),
            &GlobalArgs {},
        )
        .expect("cross-rig selected bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                for rig in result.rigs {
                    let scenarios = rig.results.expect("rig results").scenarios;
                    assert_eq!(scenarios.len(), 1);
                    assert_eq!(scenarios[0].id, "rig-slow");
                }
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn cross_rig_json_summary_omits_full_results_payload() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());

        let mut args = run_args(
            None,
            vec!["rig-a".to_string(), "rig-b".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.json_summary = true;

        let (output, exit_code) =
            run(args, &GlobalArgs {}).expect("cross-rig selected bench summary should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::ComparisonSummary(result) => {
                assert!(result.summary_only);
                assert_eq!(result.rigs.len(), 2);
                assert_eq!(result.rigs[0].rig_id, "rig-a");
                assert_eq!(result.rigs[1].rig_id, "rig-b");

                let value = serde_json::to_value(result).expect("serialize summary");
                assert!(value.get("diff").is_none());
                assert!(value["rigs"][0].get("results").is_none());
                assert!(value["rigs"][0].get("artifacts").is_none());
                assert!(value["rigs"][0].get("rig_state").is_none());
            }
            _ => panic!("expected comparison summary output"),
        }
    });
}

#[test]
fn cross_rig_reverse_order_flips_reference_and_execution_order() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());

        let mut args = run_args(
            None,
            vec!["rig-a".to_string(), "rig-b".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.rig_order = BenchRigOrder::Reverse;

        let (output, exit_code) = run(args, &GlobalArgs {})
            .expect("cross-rig selected bench should run in reverse order");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                assert_eq!(result.rigs[0].rig_id, "rig-b");
                assert_eq!(result.rigs[1].rig_id, "rig-a");
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn cross_rig_runs_preserve_run_level_summaries_after_selection() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());
        let mut args = run_args(
            None,
            vec!["rig-a".to_string(), "rig-b".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.runs = 3;

        let (output, exit_code) =
            run(args, &GlobalArgs {}).expect("cross-rig selected bench should run multiple runs");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                assert!(result
                    .summary
                    .iter()
                    .all(|summary| summary.rows.iter().all(|row| row.n == Some(3))));
                for rig in result.rigs {
                    let scenarios = rig.results.expect("rig results").scenarios;
                    assert_eq!(scenarios.len(), 1);
                    assert_eq!(scenarios[0].id, "rig-slow");
                    assert_eq!(scenarios[0].runs.as_ref().expect("runs").len(), 3);
                }
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn cross_rig_profile_requires_every_rig_to_define_profile() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig_with_profiles(
            home,
            "rig-a",
            "studio",
            component_a.path(),
            r#"{ "substrate": ["rig-extra"] }"#,
        );
        write_rig_with_profiles(
            home,
            "rig-b",
            "studio",
            component_b.path(),
            r#"{ "smoke": ["rig-slow"] }"#,
        );

        let err = match run(
            run_args_with_profile(
                None,
                vec!["rig-a".to_string(), "rig-b".to_string()],
                "substrate",
            ),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("missing cross-rig profile should fail"),
            Err(err) => err,
        };
        let message = err.to_string();

        assert!(message.contains("rig-b"), "got: {}", message);
        assert!(message.contains("substrate"), "got: {}", message);
        assert!(message.contains("smoke"), "got: {}", message);
    });
}

#[test]
fn cross_rig_profile_selects_profile_for_each_rig() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig_with_profiles(
            home,
            "rig-a",
            "studio",
            component_a.path(),
            r#"{ "substrate": ["rig-extra"] }"#,
        );
        write_rig_with_profiles(
            home,
            "rig-b",
            "studio",
            component_b.path(),
            r#"{ "substrate": ["rig-slow"] }"#,
        );

        let (output, exit_code) = run(
            run_args_with_profile(
                None,
                vec!["rig-a".to_string(), "rig-b".to_string()],
                "substrate",
            ),
            &GlobalArgs {},
        )
        .expect("cross-rig profile bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                assert_eq!(
                    result.rigs[0]
                        .results
                        .as_ref()
                        .expect("rig a results")
                        .scenarios[0]
                        .id,
                    "rig-extra"
                );
                assert_eq!(
                    result.rigs[1]
                        .results
                        .as_ref()
                        .expect("rig b results")
                        .scenarios[0]
                        .id,
                    "rig-slow"
                );
            }
            _ => panic!("expected comparison output"),
        }
    });
}
