use std::collections::BTreeMap;

use super::super::{collect_artifacts, from_main_workflow, from_main_workflow_with_rig};
use super::{
    aggregate_comparison, aggregate_comparison_with_axes, BenchComparisonDiff, BenchPhaseGroups,
    RigBenchEntry,
};
use crate::core::extension::bench::artifact::{BenchArtifact, BenchPreviewLifecycleMetadata};
use crate::core::extension::bench::diagnostic::{BenchDiagnostic, BenchDiagnosticSource};
use crate::core::extension::bench::distribution::BenchRunDistribution;
use crate::core::extension::bench::parsing::{
    BenchMetricDirection, BenchMetricPhase, BenchMetricPolicy, BenchMetrics, BenchResults,
    BenchRunSnapshot, BenchScenario,
};
use crate::core::extension::bench::run::{BenchRunFailure, BenchRunWorkflowResult};
use crate::core::extension::bench::side_by_side::BenchSideBySideMetric;

mod fixtures {
    use super::*;

    pub(super) fn scenario(id: &str, metrics: &[(&str, f64)]) -> BenchScenario {
        let mut values = BTreeMap::new();
        for (k, v) in metrics {
            values.insert((*k).to_string(), *v);
        }
        BenchScenario {
            id: id.to_string(),
            file: None,
            source: None,
            default_iterations: None,
            tags: Vec::new(),
            iterations: 10,
            metrics: BenchMetrics {
                values,
                distributions: BTreeMap::new(),
            },
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            gates: Vec::new(),
            gate_results: Vec::new(),
            metadata: BTreeMap::new(),
            passed: true,
            memory: None,
            artifacts: BTreeMap::new(),
            diagnostics: Vec::new(),
            runs: None,
            runs_summary: None,
        }
    }

    pub(super) fn scenario_with_metric_groups(
        id: &str,
        metrics: &[(&str, f64)],
        metric_groups: &[(&str, &[(&str, f64)])],
    ) -> BenchScenario {
        let mut scenario = scenario(id, metrics);
        scenario.metric_groups = metric_groups
            .iter()
            .map(|(group, values)| {
                (
                    (*group).to_string(),
                    values
                        .iter()
                        .map(|(name, value)| ((*name).to_string(), *value))
                        .collect(),
                )
            })
            .collect();
        scenario
    }

    pub(super) fn scenario_with_runs_summary(
        id: &str,
        metrics: &[(&str, f64)],
        summary_metric: &str,
        distribution: BenchRunDistribution,
    ) -> BenchScenario {
        let mut scenario = scenario(id, metrics);
        let mut runs_summary = BTreeMap::new();
        runs_summary.insert(summary_metric.to_string(), distribution);
        scenario.runs_summary = Some(runs_summary);
        scenario
    }

    pub(super) fn run_distribution(
        n: u64,
        p50: f64,
        p95: f64,
        mean: f64,
        cv_pct: f64,
    ) -> BenchRunDistribution {
        BenchRunDistribution {
            n,
            min: p50,
            max: p95,
            mean,
            stdev: mean * cv_pct / 100.0,
            cv_pct,
            p50,
            p95,
        }
    }

    pub(super) fn results(scenarios: Vec<BenchScenario>) -> BenchResults {
        BenchResults {
            component_id: "studio".to_string(),
            iterations: 10,
            run_metadata: None,
            metadata: BTreeMap::new(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: BTreeMap::new(),
            diagnostics: Vec::new(),
            phase_events: Vec::new(),
            phase_summaries: Vec::new(),
            failure_classification: None,
            budget_findings: Vec::new(),
            scenarios,
            metric_policies: BTreeMap::new(),
            metric_policy_presets: BTreeMap::new(),
        }
    }

    pub(super) fn artifact(path: &str, kind: Option<&str>, label: Option<&str>) -> BenchArtifact {
        BenchArtifact {
            path: Some(path.to_string()),
            url: None,
            artifact_type: None,
            kind: kind.map(str::to_string),
            label: label.map(str::to_string),
            observation_artifact_id: None,
            ..BenchArtifact::default()
        }
    }

    pub(super) fn artifact_with_url(
        path: &str,
        url: &str,
        kind: Option<&str>,
        label: Option<&str>,
    ) -> BenchArtifact {
        BenchArtifact {
            path: Some(path.to_string()),
            url: Some(url.to_string()),
            artifact_type: None,
            kind: kind.map(str::to_string),
            label: label.map(str::to_string),
            observation_artifact_id: None,
            ..BenchArtifact::default()
        }
    }

    pub(super) fn preview_artifact(
        role: Option<&str>,
        preview_url: &str,
        status: &str,
        expires_at: Option<&str>,
    ) -> BenchArtifact {
        BenchArtifact {
            path: Some("artifacts/preview.json".to_string()),
            url: None,
            artifact_type: Some("preview".to_string()),
            kind: Some("preview".to_string()),
            label: Some("Public preview".to_string()),
            observation_artifact_id: Some("obs-preview".to_string()),
            role: role.map(str::to_string),
            preview_url: Some(preview_url.to_string()),
            public_url: Some(preview_url.to_string()),
            local_url: Some("http://127.0.0.1:8080".to_string()),
            status: Some(status.to_string()),
            preview_lifecycle: BenchPreviewLifecycleMetadata {
                expires_at: expires_at.map(str::to_string),
                cleanup_status: Some("pending".to_string()),
                service_lifecycle: Some(serde_json::json!({
                    "service_id": "site-preview",
                    "lifecycle": status,
                    "running": status == "running"
                })),
                browser_origin_evidence: Some(serde_json::json!({
                    "browser_effective_origin": preview_url,
                    "window_is_secure_context": true
                })),
            },
        }
    }

    pub(super) fn entry(
        rig_id: &str,
        passed: bool,
        results: Option<BenchResults>,
    ) -> RigBenchEntry {
        RigBenchEntry {
            rig_id: rig_id.to_string(),
            passed,
            status: if passed { "passed" } else { "failed" }.to_string(),
            exit_code: if passed { 0 } else { 1 },
            artifacts: results.as_ref().map(collect_artifacts).unwrap_or_default(),
            results,
            rig_state: None,
            failure: None,
            diagnostics: Vec::new(),
        }
    }

    pub(super) fn failed_entry_with_stderr(rig_id: &str) -> RigBenchEntry {
        RigBenchEntry {
            rig_id: rig_id.to_string(),
            passed: false,
            status: "failed".to_string(),
            exit_code: 2,
            artifacts: Vec::new(),
            results: None,
            rig_state: None,
            failure: Some(BenchRunFailure {
                component_id: "studio".to_string(),
                component_path: Some("/Users/chubes/Developer/studio@candidate".to_string()),
                scenario_id: None,
                exit_code: 2,
                stderr_tail: "ERROR: Homeboy bench helper not found at /Users/chubes/.homeboy/runtime/bench-helper.sh".to_string(),
                diagnostics: Vec::new(),
            }),
            diagnostics: Vec::new(),
        }
    }

    pub(super) fn diagnostic(class: &str) -> BenchDiagnostic {
        BenchDiagnostic {
            class: class.to_string(),
            message: Some("database setup failed".to_string()),
            source: Some(BenchDiagnosticSource::Run),
            metadata: BTreeMap::new(),
        }
    }
}

use fixtures::*;

#[test]
fn comparison_side_by_side_renders_baseline_and_candidate_preview_links() {
    let mut baseline_scenario = scenario("site-build", &[("elapsed_ms", 12_000.0)]);
    baseline_scenario.artifacts.insert(
        "preview".to_string(),
        preview_artifact(
            None,
            "https://baseline-preview.example.test/",
            "running",
            Some("2026-06-08T12:00:00Z"),
        ),
    );

    let mut candidate_scenario = scenario("site-build", &[("elapsed_ms", 8_000.0)]);
    candidate_scenario.artifacts.insert(
        "preview".to_string(),
        preview_artifact(
            Some("candidate"),
            "https://candidate-preview.example.test/",
            "expired",
            Some("2026-06-07T12:00:00Z"),
        ),
    );

    let entries = vec![
        entry("baseline-rig", true, Some(results(vec![baseline_scenario]))),
        entry(
            "candidate-rig",
            true,
            Some(results(vec![candidate_scenario])),
        ),
    ];

    let (out, exit) = aggregate_comparison("studio".into(), 10, entries);
    let value = serde_json::to_value(&out).expect("serialize comparison");
    let report = &out.reports.side_by_side;

    assert_eq!(exit, 0);
    assert_eq!(report.rigs[0].preview_links.len(), 1);
    assert_eq!(report.rigs[0].preview_links[0].role, "baseline");
    assert_eq!(
        report.rigs[0].preview_links[0].url,
        "https://baseline-preview.example.test/"
    );
    assert_eq!(
        report.rigs[0].preview_links[0]
            .preview_lifecycle
            .expires_at
            .as_deref(),
        Some("2026-06-08T12:00:00Z")
    );
    assert_eq!(
        report.rigs[0].preview_links[0]
            .preview_lifecycle
            .service_lifecycle
            .as_ref()
            .unwrap()["service_id"],
        "site-preview"
    );
    assert_eq!(
        report.rigs[0].preview_links[0]
            .preview_lifecycle
            .browser_origin_evidence
            .as_ref()
            .unwrap()["window_is_secure_context"],
        true
    );
    assert_eq!(report.rigs[1].preview_links[0].role, "candidate");
    assert_eq!(
        report.rigs[1].preview_links[0].status.as_deref(),
        Some("expired")
    );
    assert_eq!(
        value["reports"]["side_by_side"]["rigs"][0]["preview_links"][0]["role"],
        "baseline"
    );
    assert_eq!(
        value["reports"]["side_by_side"]["rigs"][1]["preview_links"][0]["url"],
        "https://candidate-preview.example.test/"
    );
}

#[test]
fn test_from_main_workflow() {
    let (out, exit) = from_main_workflow(BenchRunWorkflowResult {
        status: "passed".to_string(),
        component: "homeboy".to_string(),
        exit_code: 0,
        iterations: 3,
        results: None,
        gate_failures: Vec::new(),
        baseline_comparison: None,
        hints: None,
        failure: None,
        diagnostics: Vec::new(),
    });

    assert!(out.passed);
    assert_eq!(out.component, "homeboy");
    assert_eq!(out.iterations, 3);
    assert_eq!(exit, 0);
}

#[test]
fn test_from_main_workflow_with_rig() {
    let (out, exit) = from_main_workflow_with_rig(
        BenchRunWorkflowResult {
            status: "failed".to_string(),
            component: "homeboy".to_string(),
            exit_code: 1,
            iterations: 1,
            results: None,
            gate_failures: Vec::new(),
            baseline_comparison: None,
            hints: Some(vec!["check output".to_string()]),
            failure: None,
            diagnostics: Vec::new(),
        },
        None,
    );

    assert!(!out.passed);
    assert_eq!(out.exit_code, 1);
    assert_eq!(out.hints.as_ref().unwrap()[0], "check output");
    assert_eq!(exit, 1);
}

#[test]
fn test_from_policies() {
    let mut policies = BTreeMap::new();
    policies.insert(
        "boot_ms".to_string(),
        BenchMetricPolicy {
            direction: BenchMetricDirection::LowerIsBetter,
            regression_threshold_percent: None,
            regression_threshold_absolute: None,
            variance_aware: false,
            min_iterations_for_variance: None,
            regression_test: None,
            phase: Some(BenchMetricPhase::Cold),
        },
    );

    let metric_names = ["boot_ms".to_string(), "p95_ms".to_string()].into();
    let groups = BenchPhaseGroups::from_policies(&policies, &metric_names);

    assert_eq!(groups.cold, vec!["boot_ms".to_string()]);
    assert_eq!(groups.untagged, vec!["p95_ms".to_string()]);
}

#[test]
fn test_is_phaseless() {
    assert!(BenchPhaseGroups {
        cold: Vec::new(),
        warm: Vec::new(),
        amortized: Vec::new(),
        untagged: vec!["p95_ms".to_string()],
    }
    .is_phaseless());

    assert!(!BenchPhaseGroups {
        cold: vec!["boot_ms".to_string()],
        warm: Vec::new(),
        amortized: Vec::new(),
        untagged: Vec::new(),
    }
    .is_phaseless());
}

#[test]
fn test_aggregate_comparison() {
    let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
    let entries = vec![entry("a", true, Some(r.clone())), entry("b", true, Some(r))];
    let (out, exit) = aggregate_comparison("studio".into(), 10, entries);

    assert!(out.passed);
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.iterations, 10);
    assert_eq!(exit, 0);
}

#[test]
fn no_axis_multi_rig_comparison_omits_axis_diffs() {
    let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
    let entries = vec![entry("a", true, Some(r.clone())), entry("b", true, Some(r))];
    let (out, _) = aggregate_comparison("studio".into(), 10, entries);
    let value = serde_json::to_value(out).expect("serialize comparison");

    assert!(value.get("axis_diffs").is_none());
}

#[test]
fn axis_diffs_cover_two_by_two_rig_matrix() {
    let entries = vec![
        entry(
            "studio-sdk-standard",
            true,
            Some(results(vec![scenario(
                "site-build",
                &[("p50_ms", 100.0), ("p95_ms", 120.0)],
            )])),
        ),
        entry(
            "studio-sdk-bfb",
            true,
            Some(results(vec![scenario(
                "site-build",
                &[("p50_ms", 80.0), ("p95_ms", 96.0)],
            )])),
        ),
        entry(
            "studio-pi-standard",
            true,
            Some(results(vec![scenario(
                "site-build",
                &[("p50_ms", 150.0), ("p95_ms", 180.0)],
            )])),
        ),
        entry(
            "studio-pi-bfb",
            true,
            Some(results(vec![scenario(
                "site-build",
                &[("p50_ms", 90.0), ("p95_ms", 108.0)],
            )])),
        ),
    ];
    let axes_by_rig: BTreeMap<String, BTreeMap<String, String>> = [
        (
            "studio-sdk-standard",
            [("runtime", "sdk"), ("substrate", "standard")],
        ),
        ("studio-sdk-bfb", [("runtime", "sdk"), ("substrate", "bfb")]),
        (
            "studio-pi-standard",
            [("runtime", "pi"), ("substrate", "standard")],
        ),
        ("studio-pi-bfb", [("runtime", "pi"), ("substrate", "bfb")]),
    ]
    .into_iter()
    .map(|(rig, axes)| {
        (
            rig.to_string(),
            axes.into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        )
    })
    .collect();

    let (out, _) = aggregate_comparison_with_axes("studio".into(), 10, entries, &axes_by_rig);

    assert_eq!(out.axis_diffs.len(), 4);
    let agent_task_plan = out.agent_task_plan.as_ref().expect("agent task plan");
    assert_eq!(agent_task_plan.plan_id, "bench/studio");
    assert_eq!(agent_task_plan.cells.len(), 4);
    let sdk_standard_cell = agent_task_plan
        .cells
        .iter()
        .find(|cell| {
            cell.axes.get("runtime").map(String::as_str) == Some("sdk")
                && cell.axes.get("substrate").map(String::as_str) == Some("standard")
        })
        .expect("sdk standard agent task cell");
    assert_eq!(
        sdk_standard_cell.task.parent_plan_id.as_deref(),
        Some("bench/studio")
    );
    assert_eq!(sdk_standard_cell.task.metadata["matrix"]["runtime"], "sdk");
    let agent_task_aggregate = out
        .agent_task_aggregate
        .as_ref()
        .expect("agent task aggregate");
    assert!(agent_task_aggregate.passed);
    assert_eq!(agent_task_aggregate.cells.len(), 4);
    assert_eq!(
        agent_task_aggregate.cells[0].status,
        Some(crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded)
    );
    let sdk_substrate = out
        .axis_diffs
        .iter()
        .find(|comparison| {
            comparison.axis == "substrate"
                && comparison.fixed.get("runtime").map(String::as_str) == Some("sdk")
        })
        .expect("runtime=sdk substrate comparison");
    assert_eq!(sdk_substrate.reference_rig, "studio-sdk-standard");
    assert_eq!(sdk_substrate.reference_value, "standard");
    assert_eq!(sdk_substrate.current_rig, "studio-sdk-bfb");
    assert_eq!(sdk_substrate.current_value, "bfb");
    let sdk_p95 = sdk_substrate.diff.by_scenario["site-build"]["p95_ms"]
        .get("studio-sdk-bfb")
        .expect("sdk bfb p95 delta");
    assert_eq!(sdk_p95.reference, 120.0);
    assert_eq!(sdk_p95.current, 96.0);
    assert!((sdk_p95.delta_percent - -20.0).abs() < 1e-9);

    let bfb_runtime = out
        .axis_diffs
        .iter()
        .find(|comparison| {
            comparison.axis == "runtime"
                && comparison.fixed.get("substrate").map(String::as_str) == Some("bfb")
        })
        .expect("substrate=bfb runtime comparison");
    assert_eq!(bfb_runtime.reference_rig, "studio-sdk-bfb");
    assert_eq!(bfb_runtime.reference_value, "sdk");
    assert_eq!(bfb_runtime.current_rig, "studio-pi-bfb");
    assert_eq!(bfb_runtime.current_value, "pi");
    let bfb_p50 = bfb_runtime.diff.by_scenario["site-build"]["p50_ms"]
        .get("studio-pi-bfb")
        .expect("bfb pi p50 delta");
    assert_eq!(bfb_p50.reference, 80.0);
    assert_eq!(bfb_p50.current, 90.0);
    assert!((bfb_p50.delta_percent - 12.5).abs() < 1e-9);
}

#[test]
fn test_collect_artifacts() {
    let mut scenario = scenario("agent-runtime", &[("p95_ms", 100.0)]);
    scenario.artifacts.insert(
        "summary".to_string(),
        artifact("artifacts/summary.json", Some("json"), Some("Summary")),
    );
    scenario.runs = Some(vec![
        BenchRunSnapshot {
            metrics: scenario.metrics.clone(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            memory: None,
            artifacts: [(
                "raw_result".to_string(),
                artifact("artifacts/run-0/raw.json", Some("json"), Some("Raw result")),
            )]
            .into(),
            diagnostics: Vec::new(),
        },
        BenchRunSnapshot {
            metrics: scenario.metrics.clone(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            memory: None,
            artifacts: [(
                "raw_result".to_string(),
                artifact("artifacts/run-1/raw.json", None, None),
            )]
            .into(),
            diagnostics: Vec::new(),
        },
    ]);

    let indexed = collect_artifacts(&results(vec![scenario]));

    assert_eq!(indexed.len(), 3);
    assert_eq!(indexed[0].scenario_id, "agent-runtime");
    assert_eq!(indexed[0].run_index, None);
    assert_eq!(indexed[0].name, "summary");
    assert_eq!(indexed[0].path.as_deref(), Some("artifacts/summary.json"));
    assert_eq!(indexed[0].kind.as_deref(), Some("json"));
    assert_eq!(indexed[0].label.as_deref(), Some("Summary"));
    assert_eq!(indexed[1].run_index, Some(0));
    assert_eq!(indexed[1].name, "raw_result");
    assert_eq!(indexed[1].path.as_deref(), Some("artifacts/run-0/raw.json"));
    assert_eq!(indexed[2].run_index, Some(1));
    assert_eq!(indexed[2].path.as_deref(), Some("artifacts/run-1/raw.json"));
}

#[test]
fn test_collect_url_artifacts() {
    let mut scenario = scenario("site-build", &[("p95_ms", 100.0)]);
    scenario.artifacts.insert(
        "frontend".to_string(),
        BenchArtifact {
            path: None,
            url: Some("https://example.test/".to_string()),
            artifact_type: Some("url".to_string()),
            kind: Some("frontend_url".to_string()),
            label: Some("Frontend".to_string()),
            observation_artifact_id: None,
            ..BenchArtifact::default()
        },
    );

    let indexed = collect_artifacts(&results(vec![scenario]));

    assert_eq!(indexed.len(), 1);
    assert_eq!(indexed[0].name, "frontend");
    assert_eq!(indexed[0].path, None);
    assert_eq!(indexed[0].url.as_deref(), Some("https://example.test/"));
    assert_eq!(indexed[0].artifact_type.as_deref(), Some("url"));
    assert_eq!(indexed[0].kind.as_deref(), Some("frontend_url"));
}

#[test]
fn test_collect_artifacts_drops_unproven_remote_absolute_paths() {
    let mut scenario = scenario("agent-runtime", &[("p95_ms", 100.0)]);
    scenario.artifacts.insert(
        "remote_trace".to_string(),
        artifact(
            "/srv/remote-run/trace.zip",
            Some("zip"),
            Some("Remote trace"),
        ),
    );
    scenario.artifacts.insert(
        "token_trace".to_string(),
        artifact(
            "runner-artifact://lab/run-1/trace.zip",
            Some("zip"),
            Some("Mirrored trace"),
        ),
    );

    let indexed = collect_artifacts(&results(vec![scenario]));

    assert_eq!(indexed.len(), 2);
    let remote = indexed
        .iter()
        .find(|artifact| artifact.name == "remote_trace")
        .expect("remote trace");
    assert_eq!(remote.path, None);
    let token = indexed
        .iter()
        .find(|artifact| artifact.name == "token_trace")
        .expect("token trace");
    assert_eq!(
        token.path.as_deref(),
        Some("runner-artifact://lab/run-1/trace.zip")
    );
}

#[test]
fn cross_rig_output_serializes_artifact_index() {
    let mut ref_scenario = scenario("agent-runtime", &[("p95_ms", 100.0)]);
    ref_scenario.runs = Some(vec![BenchRunSnapshot {
        metrics: ref_scenario.metrics.clone(),
        metric_groups: BTreeMap::new(),
        timeline: Vec::new(),
        span_definitions: Vec::new(),
        span_results: Vec::new(),
        memory: None,
        artifacts: [(
            "raw_result".to_string(),
            artifact("baseline/run-0/raw.json", None, None),
        )]
        .into(),
        diagnostics: Vec::new(),
    }]);
    let mut candidate_scenario = scenario("agent-runtime", &[("p95_ms", 80.0)]);
    candidate_scenario.runs = Some(vec![BenchRunSnapshot {
        metrics: candidate_scenario.metrics.clone(),
        metric_groups: BTreeMap::new(),
        timeline: Vec::new(),
        span_definitions: Vec::new(),
        span_results: Vec::new(),
        memory: None,
        artifacts: [(
            "raw_result".to_string(),
            artifact("candidate/run-0/raw.json", None, None),
        )]
        .into(),
        diagnostics: Vec::new(),
    }]);

    let entries = vec![
        entry("baseline", true, Some(results(vec![ref_scenario]))),
        entry("candidate", true, Some(results(vec![candidate_scenario]))),
    ];
    let (out, _) = aggregate_comparison("studio".into(), 10, entries);
    let value = serde_json::to_value(out).expect("serialize comparison");

    assert_eq!(
        value["rigs"][0]["artifacts"][0]["path"],
        "baseline/run-0/raw.json"
    );
    assert_eq!(value["rigs"][0]["artifacts"][0]["run_index"], 0);
    assert_eq!(
        value["rigs"][1]["artifacts"][0]["path"],
        "candidate/run-0/raw.json"
    );
}

#[test]
fn side_by_side_report_summarizes_multi_rig_results() {
    let mut baseline_scenario = scenario_with_metric_groups(
        "site-build",
        &[("elapsed_ms", 12_000.0), ("block_count", 42.0)],
        &[("prompt", &[("hash_match", 1.0)])],
    );
    baseline_scenario.artifacts.insert(
        "site".to_string(),
        artifact_with_url(
            "sites/baseline",
            "https://baseline.example.test",
            Some("site"),
            Some("Baseline site"),
        ),
    );

    let mut candidate_scenario = scenario_with_metric_groups(
        "site-build",
        &[("elapsed_ms", 8_000.0), ("block_count", 43.0)],
        &[("prompt", &[("hash_match", 1.0)])],
    );
    candidate_scenario.artifacts.insert(
        "site".to_string(),
        artifact_with_url(
            "sites/candidate",
            "https://candidate.example.test",
            Some("site"),
            Some("Candidate site"),
        ),
    );

    let entries = vec![
        entry(
            "studio-agent-sdk",
            true,
            Some(results(vec![baseline_scenario])),
        ),
        entry("studio-bfb", true, Some(results(vec![candidate_scenario]))),
        failed_entry_with_stderr("studio-broken"),
    ];

    let (out, exit) = aggregate_comparison("studio".into(), 10, entries);
    let report = &out.reports.side_by_side;

    assert_eq!(exit, 2);
    assert_eq!(report.report, "side_by_side");
    assert_eq!(report.component, "studio");
    assert_eq!(report.iterations, 10);
    assert_eq!(report.rigs.len(), 3);
    assert_eq!(report.rigs[0].rig_id, "studio-agent-sdk");
    assert_eq!(report.rigs[0].elapsed_ms, Some(12_000.0));
    assert!(report.rigs[0].key_metrics.contains(&BenchSideBySideMetric {
        scenario_id: "site-build".to_string(),
        name: "prompt.hash_match".to_string(),
        value: 1.0,
    }));
    assert_eq!(
        report.rigs[0].artifacts[0].url.as_deref(),
        Some("https://baseline.example.test")
    );
    assert_eq!(
        report.rigs[1].artifacts[0].url.as_deref(),
        Some("https://candidate.example.test")
    );
    assert_eq!(report.rigs[2].status, "failed");
    assert!(report.rigs[2]
        .failure_reason
        .as_deref()
        .unwrap()
        .contains("Homeboy bench helper not found"));
}

#[test]
fn diff_computes_percent_delta_lower_is_better() {
    let ref_r = results(vec![scenario("boot", &[("p95_ms", 30000.0)])]);
    let other = results(vec![scenario("boot", &[("p95_ms", 18000.0)])]);
    let diff = BenchComparisonDiff::build(("trunk", &ref_r), &[("combined-fixes", &other)]);
    let d = diff
        .by_scenario
        .get("boot")
        .and_then(|m| m.get("p95_ms"))
        .and_then(|m| m.get("combined-fixes"))
        .unwrap();
    assert_eq!(d.reference, 30000.0);
    assert_eq!(d.current, 18000.0);
    assert!((d.delta_percent - -40.0).abs() < 1e-9);
}

#[test]
fn diff_flattens_grouped_metrics_for_cross_rig_comparison() {
    let ref_r = results(vec![scenario_with_metric_groups(
        "agent",
        &[("elapsed_ms", 1000.0)],
        &[
            (
                "phases",
                &[
                    ("resolve_ai_environment_ms", 120.0),
                    ("first_assistant_message_ms", 800.0),
                ],
            ),
            ("tools", &[("max_tool_duration_ms", 250.0)]),
        ],
    )]);
    let other = results(vec![scenario_with_metric_groups(
        "agent",
        &[("elapsed_ms", 900.0)],
        &[
            (
                "phases",
                &[
                    ("resolve_ai_environment_ms", 100.0),
                    ("first_assistant_message_ms", 760.0),
                ],
            ),
            ("tools", &[("max_tool_duration_ms", 200.0)]),
        ],
    )]);

    let diff = BenchComparisonDiff::build(("ref", &ref_r), &[("next", &other)]);
    let metrics = diff.by_scenario.get("agent").expect("scenario diff");

    assert!(metrics.contains_key("elapsed_ms"));
    let phase_delta = metrics
        .get("phases.resolve_ai_environment_ms")
        .and_then(|m| m.get("next"))
        .expect("grouped phase metric diff");
    assert_eq!(phase_delta.reference, 120.0);
    assert_eq!(phase_delta.current, 100.0);
    assert!((phase_delta.delta_percent - -16.666666666666664).abs() < 1e-9);

    let tool_delta = metrics
        .get("tools.max_tool_duration_ms")
        .and_then(|m| m.get("next"))
        .expect("grouped tool metric diff");
    assert_eq!(tool_delta.reference, 250.0);
    assert_eq!(tool_delta.current, 200.0);
    assert_eq!(tool_delta.delta_percent, -20.0);
}

#[test]
fn diff_skips_missing_scenarios_silently() {
    let ref_r = results(vec![
        scenario("a", &[("p95_ms", 100.0)]),
        scenario("b", &[("p95_ms", 200.0)]),
    ]);
    let other = results(vec![scenario("a", &[("p95_ms", 110.0)])]);
    let diff = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other)]);
    assert!(diff.by_scenario.contains_key("a"));
    // "b" is in reference but absent from other; reference scenarios
    // are kept only when at least one comparison rig has the metric.
    assert!(!diff.by_scenario.contains_key("b"));
}

#[test]
fn diff_handles_zero_reference_with_signed_infinity() {
    let ref_r = results(vec![scenario("a", &[("errors", 0.0)])]);
    let other_pos = results(vec![scenario("a", &[("errors", 5.0)])]);
    let other_neg = results(vec![scenario("a", &[("errors", -5.0)])]);
    let other_zero = results(vec![scenario("a", &[("errors", 0.0)])]);

    let diff_pos = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other_pos)]);
    let pos = diff_pos
        .by_scenario
        .get("a")
        .unwrap()
        .get("errors")
        .unwrap()
        .get("other")
        .unwrap();
    assert!(pos.delta_percent.is_infinite() && pos.delta_percent.is_sign_positive());

    let diff_neg = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other_neg)]);
    let neg = diff_neg
        .by_scenario
        .get("a")
        .unwrap()
        .get("errors")
        .unwrap()
        .get("other")
        .unwrap();
    assert!(neg.delta_percent.is_infinite() && neg.delta_percent.is_sign_negative());

    let diff_zero = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other_zero)]);
    let zero = diff_zero
        .by_scenario
        .get("a")
        .unwrap()
        .get("errors")
        .unwrap()
        .get("other")
        .unwrap();
    assert_eq!(zero.delta_percent, 0.0);
}

#[test]
fn aggregate_passed_only_when_all_rigs_pass() {
    let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
    let entries = vec![
        entry("a", true, Some(r.clone())),
        entry("b", false, Some(r.clone())),
    ];
    let (out, exit) = aggregate_comparison("studio".into(), 10, entries);
    assert!(!out.passed);
    assert_eq!(exit, 1);
    assert_eq!(out.exit_code, 1);
}

#[test]
fn aggregate_exit_zero_when_all_rigs_pass() {
    let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
    let entries = vec![
        entry("a", true, Some(r.clone())),
        entry("b", true, Some(r.clone())),
    ];
    let (out, exit) = aggregate_comparison("studio".into(), 10, entries);
    assert!(out.passed);
    assert_eq!(exit, 0);
    assert_eq!(out.rigs.len(), 2);
}

#[test]
fn aggregate_handles_more_than_two_rigs() {
    let ref_r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
    let r2 = results(vec![scenario("boot", &[("p95_ms", 80.0)])]);
    let r3 = results(vec![scenario("boot", &[("p95_ms", 120.0)])]);
    let entries = vec![
        entry("a", true, Some(ref_r)),
        entry("b", true, Some(r2)),
        entry("c", true, Some(r3)),
    ];
    let (out, _) = aggregate_comparison("studio".into(), 10, entries);
    let metric = out
        .diff
        .by_scenario
        .get("boot")
        .and_then(|m| m.get("p95_ms"))
        .unwrap();
    assert!(!metric.contains_key("a")); // reference excluded
    assert_eq!(metric.len(), 2);
    assert!((metric.get("b").unwrap().delta_percent - -20.0).abs() < 1e-9);
    assert!((metric.get("c").unwrap().delta_percent - 20.0).abs() < 1e-9);
}

#[test]
fn aggregate_emits_hint_when_a_rig_has_no_results() {
    let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
    let entries = vec![entry("a", true, Some(r)), entry("b", false, None)];
    let (out, _) = aggregate_comparison("studio".into(), 10, entries);
    let hints = out.hints.as_ref().unwrap();
    assert!(hints.iter().any(|h| h.contains("no parseable results")));
}

#[test]
fn aggregate_groups_shared_diagnostic_classes_by_rig() {
    let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
    let mut baseline = entry("baseline", false, Some(r.clone()));
    baseline
        .diagnostics
        .push(diagnostic("database_unavailable"));
    let mut candidate = entry("candidate", false, Some(r));
    candidate
        .diagnostics
        .push(diagnostic("database_unavailable"));

    let (out, _) = aggregate_comparison("studio".into(), 10, vec![baseline, candidate]);

    assert_eq!(out.diagnostic_classes.len(), 1);
    assert_eq!(out.diagnostic_classes[0].class, "database_unavailable");
    assert_eq!(
        out.diagnostic_classes[0].rigs,
        vec!["baseline".to_string(), "candidate".to_string()]
    );
    assert!(out
        .hints
        .as_ref()
        .unwrap()
        .iter()
        .any(|hint| hint.contains("occurred in multiple rigs")));
}

#[test]
fn aggregate_promotes_cross_rig_run_summary() {
    let reference = results(vec![scenario_with_runs_summary(
        "studio-agent-runtime",
        &[("elapsed_ms", 7552.0), ("success_rate", 1.0)],
        "elapsed_ms",
        run_distribution(3, 7552.0, 8324.0, 7827.0, 5.27),
    )]);
    let candidate = results(vec![scenario_with_runs_summary(
        "studio-agent-runtime",
        &[("elapsed_ms", 3311.0), ("success_rate", 1.0)],
        "elapsed_ms",
        run_distribution(3, 3311.0, 3377.0, 3232.0, 5.15),
    )]);

    let entries = vec![
        entry("studio-agent-sdk", true, Some(reference)),
        entry("studio-agent-pi", true, Some(candidate)),
    ];
    let (out, _) = aggregate_comparison("studio".into(), 10, entries);

    assert_eq!(out.summary.len(), 1);
    let summary = &out.summary[0];
    assert_eq!(summary.scenario, "studio-agent-runtime");
    assert_eq!(summary.metric, "elapsed_ms");
    assert_eq!(summary.rows.len(), 2);

    let reference_row = &summary.rows[0];
    assert_eq!(reference_row.rig_id, "studio-agent-sdk");
    assert_eq!(reference_row.n, Some(3));
    assert_eq!(reference_row.p50_ms, Some(7552.0));
    assert_eq!(reference_row.p95_ms, Some(8324.0));
    assert_eq!(reference_row.mean_ms, Some(7827.0));
    assert_eq!(reference_row.cv_pct, Some(5.27));
    assert_eq!(reference_row.delta_p50_pct, Some(0.0));
    assert_eq!(
        reference_row.semantic_metrics.get("success_rate"),
        Some(&1.0)
    );

    let candidate_row = &summary.rows[1];
    assert_eq!(candidate_row.rig_id, "studio-agent-pi");
    assert_eq!(candidate_row.n, Some(3));
    assert_eq!(candidate_row.p50_ms, Some(3311.0));
    assert_eq!(candidate_row.p95_ms, Some(3377.0));
    assert_eq!(candidate_row.mean_ms, Some(3232.0));
    assert_eq!(candidate_row.cv_pct, Some(5.15));
    assert!(
        (candidate_row.delta_p50_pct.unwrap() - -56.157309322033896).abs() < 1e-9,
        "expected p50 delta against reference, got {:?}",
        candidate_row.delta_p50_pct
    );
    assert_eq!(
        candidate_row.semantic_metrics.get("success_rate"),
        Some(&1.0)
    );
}

#[test]
fn comparison_summary_serializes_as_direct_table_shape() {
    let reference = results(vec![scenario_with_runs_summary(
        "chat",
        &[("elapsed_ms", 100.0), ("assistant_message_count", 2.0)],
        "elapsed_ms",
        run_distribution(2, 100.0, 110.0, 105.0, 4.76),
    )]);
    let candidate = results(vec![scenario_with_runs_summary(
        "chat",
        &[("elapsed_ms", 80.0), ("assistant_message_count", 2.0)],
        "elapsed_ms",
        run_distribution(2, 80.0, 90.0, 85.0, 5.88),
    )]);

    let entries = vec![
        entry("ref", true, Some(reference)),
        entry("next", true, Some(candidate)),
    ];
    let (out, _) = aggregate_comparison("agent".into(), 10, entries);
    let value = serde_json::to_value(out).unwrap();
    let rows = value["summary"][0]["rows"].as_array().unwrap();

    assert_eq!(value["summary"][0]["scenario"], "chat");
    assert_eq!(rows[0]["rig_id"], "ref");
    assert_eq!(rows[0]["n"], 2);
    assert_eq!(rows[0]["p50_ms"], 100.0);
    assert_eq!(rows[0]["p95_ms"], 110.0);
    assert_eq!(rows[0]["mean_ms"], 105.0);
    assert_eq!(rows[0]["cv_pct"], 4.76);
    assert_eq!(rows[0]["assistant_message_count"], 2.0);
    assert_eq!(rows[1]["rig_id"], "next");
    assert_eq!(rows[1]["delta_p50_pct"], -20.0);
}

#[test]
fn aggregate_surfaces_no_parseable_failure_metadata() {
    let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
    let entries = vec![
        entry("baseline", true, Some(r)),
        failed_entry_with_stderr("candidate"),
    ];
    let (out, exit) = aggregate_comparison("studio".into(), 10, entries);

    assert_eq!(exit, 2);
    assert_eq!(out.failures.len(), 1);
    let failure = &out.failures[0];
    assert_eq!(failure.rig_id, "candidate");
    assert_eq!(failure.component_id, "studio");
    assert_eq!(failure.exit_code, 2);
    assert!(failure
        .stderr_tail
        .contains("Homeboy bench helper not found"));

    let value = serde_json::to_value(&out).unwrap();
    let json_failure = &value["failures"][0];
    assert_eq!(json_failure["rig_id"], "candidate");
    assert_eq!(json_failure["component_id"], "studio");
    assert!(json_failure["stderr_tail"]
        .as_str()
        .unwrap()
        .contains("bench-helper.sh"));
}

#[test]
fn aggregate_puts_actionable_failure_block_before_generic_hint() {
    let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
    let entries = vec![
        entry("baseline", true, Some(r)),
        failed_entry_with_stderr("candidate"),
    ];
    let (out, _) = aggregate_comparison("studio".into(), 10, entries);
    let hints = out.hints.as_ref().unwrap();

    assert!(hints[0].starts_with("Rig failed before producing parseable bench results:"));
    assert!(hints[0].contains("- rig: candidate"));
    assert!(hints[0].contains("- component: studio (/Users/chubes/Developer/studio@candidate)"));
    assert!(hints[0].contains("- exit: 2"));
    assert!(hints[0].contains("Homeboy bench helper not found"));
    assert!(hints[1].contains("no parseable results"));
}
