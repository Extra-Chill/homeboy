use super::*;

#[test]
fn single_rig_run_records_installed_rig_package_evidence_in_metadata() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        let package_root = install_rig_package(home, "studio-bfb", "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(None, vec!["studio-bfb".to_string()], Vec::new()),
            &GlobalArgs {},
        )
        .expect("rig bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let package_root = package_root.to_string_lossy().to_string();
                let metadata = result
                    .results
                    .expect("bench results")
                    .run_metadata
                    .expect("run metadata");
                let evidence = metadata.rig_package.expect("rig package evidence");
                assert_eq!(evidence.package_root, package_root);
                assert_eq!(evidence.rig_id, "studio-bfb");
                assert_eq!(evidence.source_root.as_deref(), Some(package_root.as_str()));
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn local_git_rig_package_evidence_records_source_identity() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        let (_package_root, revision) =
            install_git_rig_package(home, "studio-bfb", "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(None, vec!["studio-bfb".to_string()], Vec::new()),
            &GlobalArgs {},
        )
        .expect("rig bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let metadata = result
                    .results
                    .expect("bench results")
                    .run_metadata
                    .expect("run metadata");
                let evidence = metadata.rig_package.expect("rig package evidence");
                assert_eq!(
                    evidence.installed_source_revision.as_deref(),
                    Some(revision.as_str())
                );
                assert_eq!(
                    evidence.current_source_revision.as_deref(),
                    Some(revision.as_str())
                );
                assert_eq!(evidence.source_ref.as_deref(), Some("main"));
                assert!(evidence.source_dirty);
                assert!(evidence.freshness_verified);
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn run_selects_single_scenario() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(Some("studio"), Vec::new(), vec!["slow".to_string()]),
            &GlobalArgs {},
        )
        .expect("selected bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let scenarios = result.results.expect("results").scenarios;
                assert_eq!(scenarios.len(), 1);
                assert_eq!(scenarios[0].id, "slow");
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn selected_scenario_workload_failure_preserves_runner_error() {
    with_isolated_home(|home| {
        write_failing_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(
                Some("studio"),
                Vec::new(),
                vec!["studio-agent-site-build".to_string()],
            ),
            &GlobalArgs {},
        )
        .expect("runner failure should return structured bench output");

        assert_eq!(exit_code, 7);
        match output {
            BenchOutput::Single(result) => {
                assert_eq!(result.status, "failed");
                assert_eq!(result.exit_code, 7);
                assert_eq!(result.results.expect("partial results").scenarios.len(), 0);
                let failure = result.failure.expect("runner failure metadata");
                assert_eq!(
                    failure.scenario_id.as_deref(),
                    Some("studio-agent-site-build")
                );
                assert!(failure.stderr_tail.contains("WORKLOAD_ERROR"));
                assert!(failure.stderr_tail.contains("warmup iteration 1/1"));
                assert!(failure.stderr_tail.contains("component id `studio`"));
                assert!(failure
                    .stderr_tail
                    .contains("workload id `studio-agent-site-build`"));
                assert!(failure
                    .stderr_tail
                    .contains("scenario id `studio-agent-site-build`"));
                assert!(failure.stderr_tail.contains("phase `warmup`"));
                assert!(failure.stderr_tail.contains("iteration 1/1"));
                assert!(failure
                    .stderr_tail
                    .contains("artifact key `visual_comparison_dir`"));
                assert!(failure.stderr_tail.contains("Omit optional artifacts"));
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn run_selects_multiple_scenarios() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(
                Some("studio"),
                Vec::new(),
                vec!["in-tree".to_string(), "slow".to_string()],
            ),
            &GlobalArgs {},
        )
        .expect("selected bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let scenario_ids: Vec<String> = result
                    .results
                    .expect("results")
                    .scenarios
                    .into_iter()
                    .map(|scenario| scenario.id)
                    .collect();
                assert_eq!(
                    scenario_ids,
                    vec!["in-tree".to_string(), "slow".to_string()]
                );
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn unknown_scenario_reports_discovered_ids() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let err = match run(
            run_args(Some("studio"), Vec::new(), vec!["missing".to_string()]),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("unknown scenario should fail"),
            Err(err) => err,
        };
        let message = err.to_string();

        assert!(message.contains("missing"), "got: {}", message);
        assert!(message.contains("in-tree"), "got: {}", message);
        assert!(message.contains("slow"), "got: {}", message);
    });
}

#[test]
fn single_rig_selector_filters_extra_workloads_before_execution() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "rig-a", "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(
                None,
                vec!["rig-a".to_string()],
                vec!["rig-slow".to_string()],
            ),
            &GlobalArgs {},
        )
        .expect("single-rig selected bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                assert_eq!(result.iterations, 1);
                let scenarios = result.results.expect("results").scenarios;
                assert_eq!(scenarios.len(), 1);
                assert_eq!(scenarios[0].id, "rig-slow");
                assert_eq!(scenarios[0].iterations, 1);
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn rig_bench_component_expands_extension_settings() {
    let monorepo = tempfile::TempDir::new().expect("monorepo dir");
    let plugin_path = monorepo.path().join("plugins").join("woocommerce");
    fs::create_dir_all(&plugin_path).expect("create plugin path");

    let spec: RigSpec = serde_json::from_value(serde_json::json!({
        "id": "example-performance",
        "components": {
            "example": {
                "path": plugin_path.to_string_lossy(),
                "extensions": {
                    "example-extension": {
                        "source_root": "${components.example.path}/../..",
                        "source_subpath": "plugins/example",
                        "file_mounts": [
                            { "source": "${components.example.path}/example.php" }
                        ]
                    }
                }
            }
        }
    }))
    .expect("parse rig spec");

    let component =
        matrix::rig_component_for_bench(&spec, "example").expect("resolve rig bench component");
    let extension = component
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("example-extension"))
        .expect("example extension settings");

    assert_eq!(
        extension.settings.get("source_root"),
        Some(&serde_json::Value::String(
            plugin_path.join("../..").to_string_lossy().into_owned()
        ))
    );
    let expected_mount_source = plugin_path
        .join("example.php")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        extension
            .settings
            .get("file_mounts")
            .and_then(|value| value.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("source"))
            .and_then(|value| value.as_str()),
        Some(expected_mount_source.as_str())
    );
}

#[test]
fn run_prefers_rig_workloads_over_component_bench_script() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig_with_component_script(home, "studio-bfb", "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(None, vec!["studio-bfb".to_string()], Vec::new()),
            &GlobalArgs {},
        )
        .expect("rig bench should use rig workloads");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let scenarios = result.results.expect("results").scenarios;
                assert_eq!(scenarios.len(), 1);
                assert_eq!(scenarios[0].id, "rig-extra");
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn run_profile_selects_rig_profile_scenarios() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig_with_profiles(
            home,
            "studio-bfb",
            "studio",
            component_dir.path(),
            r#"{ "substrate": ["rig-extra"] }"#,
        );

        let (output, exit_code) = run(
            run_args_with_profile(None, vec!["studio-bfb".to_string()], "substrate"),
            &GlobalArgs {},
        )
        .expect("profile bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let scenarios = result.results.expect("results").scenarios;
                assert_eq!(scenarios.len(), 1);
                assert_eq!(scenarios[0].id, "rig-extra");
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn single_rig_runs_preserve_run_level_summaries_after_selection() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "rig-a", "studio", component_dir.path());
        let mut args = run_args(
            None,
            vec!["rig-a".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.runs = 3;

        let (output, exit_code) =
            run(args, &GlobalArgs {}).expect("single-rig selected bench should run multiple runs");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let scenarios = result.results.expect("results").scenarios;
                assert_eq!(scenarios.len(), 1);
                let runs = scenarios[0].runs.as_ref().expect("per-run metrics");
                assert_eq!(runs.len(), 3);
                assert!(scenarios[0].runs_summary.as_ref().is_some_and(|summary| {
                    summary.get("p95_ms").is_some_and(|metric| metric.n == 3)
                }));
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn unknown_profile_lists_available_profiles() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig_with_profiles(
            home,
            "studio-bfb",
            "studio",
            component_dir.path(),
            r#"{ "substrate": ["rig-extra"], "smoke": ["rig-slow"] }"#,
        );

        let err = match run(
            run_args_with_profile(None, vec!["studio-bfb".to_string()], "agentic"),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("unknown profile should fail"),
            Err(err) => err,
        };
        let message = err.to_string();

        assert!(message.contains("agentic"), "got: {}", message);
        assert!(message.contains("substrate"), "got: {}", message);
        assert!(message.contains("smoke"), "got: {}", message);
    });
}

#[test]
fn profile_with_unknown_scenario_lists_discovered_scenarios() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig_with_profiles(
            home,
            "studio-bfb",
            "studio",
            component_dir.path(),
            r#"{ "broken": ["missing-scenario"] }"#,
        );

        let err = match run(
            run_args_with_profile(None, vec!["studio-bfb".to_string()], "broken"),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("unknown scenario in profile should fail"),
            Err(err) => err,
        };
        let message = err.to_string();

        assert!(message.contains("missing-scenario"), "got: {}", message);
        assert!(message.contains("rig-extra"), "got: {}", message);
        assert!(message.contains("rig-slow"), "got: {}", message);
    });
}
