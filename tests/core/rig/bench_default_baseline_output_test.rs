use super::*;

fn write_rig_with_default_baseline(
    home: &TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
    default_baseline_rig: &str,
) {
    write_rig(home, rig_id, component_id, path);
    let rig_path = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("rigs")
        .join(format!("{}.json", rig_id));
    let mut rig_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&rig_path).expect("read rig")).expect("parse rig");
    rig_json["bench"]["default_baseline_rig"] = serde_json::json!(default_baseline_rig);
    fs::write(
        &rig_path,
        serde_json::to_string(&rig_json).expect("serialize rig"),
    )
    .expect("write rig");
}

#[test]
fn default_baseline_expansion_records_metadata_on_comparison_output() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let baseline_component = tempfile::TempDir::new().expect("baseline component");
        let candidate_component = tempfile::TempDir::new().expect("candidate component");
        write_rig(
            home,
            "studio-agent-sdk",
            "studio",
            baseline_component.path(),
        );
        write_rig_with_default_baseline(
            home,
            "studio-bfb",
            "studio",
            candidate_component.path(),
            "studio-agent-sdk",
        );

        let (output, exit_code) = run(
            run_args(
                None,
                vec!["studio-bfb".to_string()],
                vec!["rig-slow".to_string()],
            ),
            &GlobalArgs {},
        )
        .expect("default baseline expansion should run as comparison");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                let expansion = result
                    .default_baseline_expansion
                    .expect("expansion metadata");
                assert_eq!(expansion.baseline_rig, "studio-agent-sdk");
                assert_eq!(expansion.candidate_rig, "studio-bfb");
                assert_eq!(
                    expansion.execution_order,
                    vec!["studio-agent-sdk".to_string(), "studio-bfb".to_string()]
                );
                assert_eq!(expansion.opt_out_flag, "--ignore-default-baseline");
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn default_baseline_expansion_metadata_survives_json_summary() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let baseline_component = tempfile::TempDir::new().expect("baseline component");
        let candidate_component = tempfile::TempDir::new().expect("candidate component");
        write_rig(
            home,
            "studio-agent-sdk",
            "studio",
            baseline_component.path(),
        );
        write_rig_with_default_baseline(
            home,
            "studio-bfb",
            "studio",
            candidate_component.path(),
            "studio-agent-sdk",
        );

        let mut args = run_args(
            None,
            vec!["studio-bfb".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.json_summary = true;
        let (output, exit_code) =
            run(args, &GlobalArgs {}).expect("default baseline summary should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::ComparisonSummary(result) => {
                let value = serde_json::to_value(result).expect("serialize summary");
                assert_eq!(
                    value["default_baseline_expansion"]["baseline_rig"],
                    "studio-agent-sdk"
                );
                assert_eq!(
                    value["default_baseline_expansion"]["candidate_rig"],
                    "studio-bfb"
                );
                assert_eq!(
                    value["default_baseline_expansion"]["execution_order"],
                    serde_json::json!(["studio-agent-sdk", "studio-bfb"])
                );
                assert_eq!(
                    value["default_baseline_expansion"]["opt_out_flag"],
                    "--ignore-default-baseline"
                );
            }
            _ => panic!("expected comparison summary output"),
        }
    });
}

#[test]
fn default_baseline_expansion_notice_names_order_and_opt_out() {
    let metadata = BenchDefaultBaselineExpansion {
        baseline_rig: "studio-agent-sdk".to_string(),
        candidate_rig: "studio-bfb".to_string(),
        execution_order: vec!["studio-agent-sdk".to_string(), "studio-bfb".to_string()],
        opt_out_flag: "--ignore-default-baseline",
    };

    let notice = default_baseline_notice(&metadata);

    assert!(notice.contains("studio-agent-sdk"), "got: {notice}");
    assert!(notice.contains("studio-bfb"), "got: {notice}");
    assert!(
        notice.contains("studio-agent-sdk -> studio-bfb"),
        "got: {notice}"
    );
    assert!(
        notice.contains("--ignore-default-baseline"),
        "got: {notice}"
    );
}

#[test]
fn default_baseline_failure_summary_marks_implicit_baseline() {
    let metadata = BenchDefaultBaselineExpansion {
        baseline_rig: "studio-agent-sdk".to_string(),
        candidate_rig: "studio-bfb".to_string(),
        execution_order: vec!["studio-agent-sdk".to_string(), "studio-bfb".to_string()],
        opt_out_flag: "--ignore-default-baseline",
    };
    let entries = vec![
        RigBenchEntry {
            rig_id: "studio-agent-sdk".to_string(),
            passed: false,
            status: "failed".to_string(),
            exit_code: 7,
            artifacts: Vec::new(),
            results: None,
            rig_state: None,
            failure: Some(homeboy::core::extension::bench::run::BenchRunFailure {
                component_id: "studio".to_string(),
                component_path: None,
                scenario_id: None,
                exit_code: 7,
                stderr_tail: "baseline setup failed".to_string(),
                failure_classification: None,
                responsiveness: None,
                memory_sample: None,
                diagnostics: Vec::new(),
            }),
            diagnostics: Vec::new(),
        },
        RigBenchEntry {
            rig_id: "studio-bfb".to_string(),
            passed: true,
            status: "passed".to_string(),
            exit_code: 0,
            artifacts: Vec::new(),
            results: None,
            rig_state: None,
            failure: None,
            diagnostics: Vec::new(),
        },
    ];
    let (mut output, _) = aggregate_comparison("studio".to_string(), 10, entries);

    apply_default_baseline_failure_context(&mut output, &metadata);

    assert!(output.failures[0].implicit_default_baseline);
    let hints = output.hints.as_ref().expect("hints");
    assert!(hints[0].contains("Implicit default baseline rig 'studio-agent-sdk'"));
    assert!(hints[0].contains("requested rig 'studio-bfb'"));

    let value = serde_json::to_value(&output).expect("serialize output");
    assert_eq!(
        value["failures"][0]["implicit_default_baseline"],
        serde_json::Value::Bool(true)
    );
}

#[test]
fn default_baseline_early_error_hint_names_requested_rig() {
    let metadata = BenchDefaultBaselineExpansion {
        baseline_rig: "studio-agent-sdk".to_string(),
        candidate_rig: "studio-bfb".to_string(),
        execution_order: vec!["studio-agent-sdk".to_string(), "studio-bfb".to_string()],
        opt_out_flag: "--ignore-default-baseline",
    };
    let error = homeboy::core::Error::rig_pipeline_failed(
        "studio-agent-sdk",
        "check",
        "rig check failed; refusing to run bench against an unhealthy rig",
    );

    let error = add_default_baseline_failure_hint(error, Some(&metadata));

    assert_eq!(error.hints.len(), 1);
    assert!(error.hints[0]
        .message
        .contains("failed before requested rig 'studio-bfb'"));
    assert!(error.hints[0].message.contains("--ignore-default-baseline"));
}
