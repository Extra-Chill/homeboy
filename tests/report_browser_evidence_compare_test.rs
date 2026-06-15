use std::fs;
use std::path::{Path, PathBuf};

use homeboy::commands::report::{
    browser_evidence_compare_from_args, browser_evidence_compare_from_dirs,
    browser_evidence_compare_from_dirs_with_visual_and_adapters, BrowserEvidenceCompareArgs,
    VisualCompareOptions,
};
use homeboy::core::extension::{
    TraceBrowserArtifactMapConfig, TraceBrowserEvidenceAdapterConfig,
    TraceBrowserMetricAliasConfig, TraceBrowserSummaryAliasConfig,
};

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-browser-evidence-compare-{name}-{nanos}"))
}

fn write_fixture_file(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir).expect("fixture dir should exist");
    let path = dir.join(name);
    fs::write(&path, body).unwrap_or_else(|err| {
        panic!(
            "failed to write browser evidence fixture {}: {}",
            path.display(),
            err
        )
    });
}

fn browser_evidence_adapter() -> TraceBrowserEvidenceAdapterConfig {
    TraceBrowserEvidenceAdapterConfig {
        id: "custom-provider.browser-summary".to_string(),
        summary_aliases: vec![TraceBrowserSummaryAliasConfig {
            request_total_keys: vec!["networkEvents".to_string()],
            page_error_keys: vec!["errors".to_string()],
            metrics: vec![
                TraceBrowserMetricAliasConfig {
                    metric: "browser_console_message_count".to_string(),
                    keys: vec!["consoleMessages".to_string()],
                },
                TraceBrowserMetricAliasConfig {
                    metric: "browser_page_error_count".to_string(),
                    keys: vec!["errors".to_string()],
                },
                TraceBrowserMetricAliasConfig {
                    metric: "browser_network_event_count".to_string(),
                    keys: vec!["networkEvents".to_string()],
                },
            ],
        }],
        artifact_maps: vec![TraceBrowserArtifactMapConfig {
            field: "files".to_string(),
        }],
    }
}

fn args(root: &Path, include_local_paths: bool) -> BrowserEvidenceCompareArgs {
    BrowserEvidenceCompareArgs {
        baseline_dir: root.join("baseline").to_string_lossy().to_string(),
        candidate_dir: root.join("candidate").to_string_lossy().to_string(),
        baseline_label: "baseline-main".to_string(),
        candidate_label: "candidate-pr".to_string(),
        include_local_paths,
        format: "markdown".to_string(),
        visual_compare: false,
        visual_artifacts_dir: None,
        visual_compare_provider: None,
        visual_provider_args: Vec::new(),
        visual_threshold: None,
    }
}

#[test]
fn compares_repeats_matrix_variants_and_browser_deltas() {
    let root = tmp_dir("full");
    write_fixture_file(
        &root.join("baseline"),
        "browser-evidence.json",
        r#"{
            "data": {
                "scenarios": [{
                    "id": "editor-load",
                    "profile": "desktop",
                    "matrix": { "theme": "twentytwentyfour" },
                    "runs": [{
                        "assertions": [{"id":"ready","status":"pass"},{"id":"console","status":"pass"}],
                        "requests": [
                            {"url":"https://example.test/wp-admin/","resource_type":"document"},
                            {"url":"https://example.test/app.js","resource_type":"script"},
                            {"url":"https://cdn.example.test/style.css","resource_type":"stylesheet"}
                        ],
                        "browser_metrics": { "lcp_ms": 1200, "ready_ms": 800 },
                        "dom_lifecycle": { "dom_content_loaded_ms": 700, "load_event_ms": 900 },
                        "console_errors": [],
                        "page_errors": [],
                        "artifacts": [{"label":"trace","url":"https://artifacts.example.test/baseline-trace.zip"}]
                    },{
                        "assertions": [{"id":"ready","status":"pass"},{"id":"console","status":"pass"}],
                        "requests": [
                            {"url":"https://example.test/wp-admin/","resource_type":"document"},
                            {"url":"https://example.test/app.js","resource_type":"script"},
                            {"url":"https://example.test/extra.js","resource_type":"script"},
                            {"url":"https://cdn.example.test/style.css","resource_type":"stylesheet"}
                        ],
                        "browser_metrics": { "lcp_ms": 1400, "ready_ms": 1000 },
                        "dom_lifecycle": { "dom_content_loaded_ms": 800, "load_event_ms": 1100 },
                        "console_errors": [],
                        "page_errors": []
                    }]
                },{
                    "id": "editor-load",
                    "profile": "mobile",
                    "matrix": { "theme": "twentytwentyfour" },
                    "assertions": [{"id":"ready","status":"pass"}],
                    "request_summary": { "total": 2, "by_host": {"example.test": 2}, "by_type": {"document": 1, "script": 1} },
                    "browser_metrics": { "lcp_ms": 2000 },
                    "dom_lifecycle": { "dom_content_loaded_ms": 1200 },
                    "console_errors": 0,
                    "page_errors": 0
                }]
            }
        }"#,
    );
    write_fixture_file(
        &root.join("candidate"),
        "browser-evidence.json",
        r#"{
            "data": {
                "scenarios": [{
                    "id": "editor-load",
                    "profile": "desktop",
                    "matrix": { "theme": "twentytwentyfour" },
                    "runs": [{
                        "assertions": [{"id":"ready","status":"pass"},{"id":"console","status":"fail"}],
                        "requests": [
                            {"url":"https://example.test/wp-admin/","resource_type":"document"},
                            {"url":"https://example.test/app.js","resource_type":"script"},
                            {"url":"https://example.test/new.js","resource_type":"script"},
                            {"url":"https://cdn.example.test/style.css","resource_type":"stylesheet"}
                        ],
                        "browser_metrics": { "lcp_ms": 1600, "ready_ms": 1200 },
                        "dom_lifecycle": { "dom_content_loaded_ms": 1000, "load_event_ms": 1400 },
                        "console_errors": [{"text":"boom"}],
                        "page_errors": []
                    },{
                        "assertions": [{"id":"ready","status":"pass"},{"id":"console","status":"pass"}],
                        "requests": [
                            {"url":"https://example.test/wp-admin/","resource_type":"document"},
                            {"url":"https://example.test/app.js","resource_type":"script"},
                            {"url":"https://example.test/new.js","resource_type":"script"},
                            {"url":"https://example.test/extra.js","resource_type":"script"},
                            {"url":"https://cdn.example.test/style.css","resource_type":"stylesheet"}
                        ],
                        "browser_metrics": { "lcp_ms": 1800, "ready_ms": 1400 },
                        "dom_lifecycle": { "dom_content_loaded_ms": 1100, "load_event_ms": 1500 },
                        "console_errors": [],
                        "page_errors": [{"message":"candidate page error"}],
                        "artifacts": [{"label":"screenshot","path":"screens/final.png"}]
                    }]
                }]
            }
        }"#,
    );

    let report = browser_evidence_compare_from_args(&args(&root, false)).expect("report renders");

    assert_eq!(report.totals.baseline_samples, 3);
    assert_eq!(report.totals.candidate_samples, 2);
    assert_eq!(report.totals.variant_count, 2);
    let desktop = report
        .variants
        .iter()
        .find(|variant| variant.variant.profile == "desktop")
        .expect("desktop variant should exist");
    assert_eq!(desktop.baseline_repeats, 2);
    assert_eq!(desktop.candidate_repeats, 2);
    assert_eq!(desktop.assertions.fail_delta, 1);
    assert_eq!(
        desktop.request_totals.baseline.as_ref().unwrap().median,
        3.5
    );
    assert_eq!(
        desktop.request_totals.candidate.as_ref().unwrap().median,
        4.5
    );
    assert_eq!(desktop.request_totals.median_delta, Some(1.0));
    assert_eq!(desktop.console_errors.median_delta, Some(0.5));
    assert_eq!(desktop.page_errors.median_delta, Some(0.5));
    assert_eq!(desktop.browser_metrics["lcp_ms"].median_delta, Some(400.0));
    assert_eq!(
        desktop.lifecycle_metrics["dom_content_loaded_ms"].median_delta,
        Some(300.0)
    );
    assert!(desktop.request_by_host.contains_key("example.test"));
    assert!(desktop.request_by_type.contains_key("script"));
    assert!(report.markdown.contains("## Browser Evidence Comparison"));
    assert!(report
        .markdown
        .contains("### Scenario / Profile Matrix Summary"));
    assert!(report.markdown.contains("theme=twentytwentyfour"));
    assert!(report.markdown.contains("Request Counts By Host"));
    assert!(report.markdown.contains("DOM Lifecycle Metrics"));
    assert!(report.markdown.contains("browser-evidence.json"));
    assert!(!report.markdown.contains(root.to_string_lossy().as_ref()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn can_include_local_paths_when_requested() {
    let root = tmp_dir("local-paths");
    write_fixture_file(
        &root.join("baseline"),
        "evidence.json",
        r#"{"scenario_id":"home","assertions":[],"request_count":1}"#,
    );
    write_fixture_file(
        &root.join("candidate"),
        "evidence.json",
        r#"{"scenario_id":"home","assertions":[],"request_count":2}"#,
    );

    let report = browser_evidence_compare_from_args(&args(&root, true)).expect("report renders");

    assert!(report.markdown.contains(root.to_string_lossy().as_ref()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn compares_browser_evidence_across_multiple_run_dirs() {
    let root = tmp_dir("multi-dirs");
    write_fixture_file(
        &root.join("baseline-run-1"),
        "browser.json",
        r#"{"scenario_id":"checkout","profile":"throttled-mobile","browser_metrics":{"ready_ms":900}}"#,
    );
    write_fixture_file(
        &root.join("baseline-run-2"),
        "browser.json",
        r#"{"scenario_id":"checkout","profile":"throttled-mobile","browser_metrics":{"ready_ms":1100}}"#,
    );
    write_fixture_file(
        &root.join("candidate-run-1"),
        "browser.json",
        r#"{"scenario_id":"checkout","profile":"throttled-mobile","browser_metrics":{"ready_ms":700}}"#,
    );
    write_fixture_file(
        &root.join("candidate-run-2"),
        "browser.json",
        r#"{"scenario_id":"checkout","profile":"throttled-mobile","browser_metrics":{"ready_ms":900}}"#,
    );

    let report = browser_evidence_compare_from_dirs(
        &[root.join("baseline-run-1"), root.join("baseline-run-2")],
        &[root.join("candidate-run-1"), root.join("candidate-run-2")],
        "baseline-ref",
        "candidate-ref",
        false,
    )
    .expect("multi-dir report renders");

    assert_eq!(report.totals.baseline_samples, 2);
    assert_eq!(report.totals.candidate_samples, 2);
    assert_eq!(report.variants.len(), 1);
    assert_eq!(
        report.variants[0].browser_metrics["ready_ms"].median_delta,
        Some(-200.0)
    );
    assert!(report.markdown.contains("throttled-mobile"));
    assert!(report.markdown.contains("baseline-ref"));
    assert!(report.markdown.contains("candidate-ref"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn surfaces_failed_advisory_assertions_in_json_and_markdown() {
    let root = tmp_dir("advisory-assertions");
    write_fixture_file(
        &root.join("baseline"),
        "browser-evidence.json",
        r##"{
            "scenario_id":"checkout",
            "profile":"desktop",
            "assertions":[
                {"id":"advisory:exists:#wc-stripe-express-checkout-element","status":"pass","selector":"#wc-stripe-express-checkout-element"}
            ],
            "browser_metrics":{"ready_ms":900}
        }"##,
    );
    write_fixture_file(
        &root.join("candidate"),
        "browser-evidence.json",
        r##"{
            "scenario_id":"checkout",
            "profile":"desktop",
            "assertions":[
                {
                    "id":"advisory:exists:#wc-stripe-express-checkout-element",
                    "status":"fail",
                    "selector":"#wc-stripe-express-checkout-element",
                    "message":"expected ECE mount to exist"
                }
            ],
            "browser_metrics":{"ready_ms":900}
        }"##,
    );

    let report = browser_evidence_compare_from_args(&args(&root, false)).expect("report renders");
    let variant = report.variants.first().expect("variant should exist");

    assert_eq!(variant.assertions.baseline.advisory_failed, 0);
    assert_eq!(variant.assertions.candidate.advisory_failed, 1);
    assert_eq!(
        variant.assertions.candidate.failed_advisory_assertions[0]
            .selector
            .as_deref(),
        Some("#wc-stripe-express-checkout-element")
    );
    assert!(report
        .markdown
        .contains("Candidate failed advisory assertions"));
    assert!(report
        .markdown
        .contains("`advisory:exists:#wc-stripe-express-checkout-element`"));
    assert!(report
        .markdown
        .contains("selector `#wc-stripe-express-checkout-element`"));

    let report_json = serde_json::to_value(&report).expect("report should serialize");
    assert_eq!(
        report_json["variants"][0]["assertions"]["candidate"]["advisory_failed"],
        serde_json::json!(1)
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn promotes_declared_browser_summary_metrics() {
    let root = tmp_dir("provider-summary");
    write_fixture_file(
        &root.join("baseline"),
        "summary.json",
        r#"{
            "schema":"custom-provider/browser-probe/v1",
            "summary": {
                "assertions": {"total": 1, "passed": 1, "failed": 0, "skipped": 0},
                "consoleMessages": 0,
                "errors": 0,
                "networkEvents": 12,
                "metrics": {
                    "browser_lcp_ms": 1200,
                    "browser_peak_used_js_heap_bytes": 64000,
                    "browser_transfer_size_bytes": 100000,
                    "browser_dom_node_count": 320
                }
            },
            "files": {
                "summary": "files/browser/summary.json",
                "performance": "files/browser/performance.json",
                "memory": "files/browser/memory.json",
                "screenshot": "files/browser/screenshot.png"
            }
        }"#,
    );
    write_fixture_file(
        &root.join("candidate"),
        "summary.json",
        r#"{
            "schema":"custom-provider/browser-probe/v1",
            "summary": {
                "assertions": {"total": 1, "passed": 1, "failed": 0, "skipped": 0},
                "consoleMessages": 1,
                "errors": 1,
                "networkEvents": 14,
                "metrics": {
                    "browser_lcp_ms": 1500,
                    "browser_peak_used_js_heap_bytes": 96000,
                    "browser_transfer_size_bytes": 140000,
                    "browser_dom_node_count": 400
                }
            },
            "files": {
                "summary": "files/browser/summary.json",
                "performance": "files/browser/performance.json",
                "memory": "files/browser/memory.json",
                "screenshot": "files/browser/screenshot.png"
            }
        }"#,
    );

    let report = browser_evidence_compare_from_dirs_with_visual_and_adapters(
        &[root.join("baseline")],
        &[root.join("candidate")],
        "baseline-main",
        "candidate-pr",
        false,
        None,
        &[browser_evidence_adapter()],
    )
    .expect("report renders");
    let variant = report.variants.first().expect("variant should exist");

    assert_eq!(report.totals.baseline_samples, 1);
    assert_eq!(report.totals.candidate_samples, 1);
    assert_eq!(variant.assertions.pass_delta, 0);
    assert_eq!(variant.request_totals.median_delta, Some(2.0));
    assert_eq!(variant.console_errors.median_delta, None);
    assert_eq!(variant.page_errors.median_delta, Some(1.0));
    assert_eq!(
        variant.browser_metrics["browser_lcp_ms"].median_delta,
        Some(300.0)
    );
    assert_eq!(
        variant.browser_metrics["browser_peak_used_js_heap_bytes"].median_delta,
        Some(32000.0)
    );
    assert_eq!(
        variant.browser_metrics["browser_console_message_count"].median_delta,
        Some(1.0)
    );
    assert_eq!(
        variant.browser_metrics["browser_network_event_count"].median_delta,
        Some(2.0)
    );
    assert!(variant
        .artifacts
        .baseline
        .iter()
        .any(|artifact| artifact.label == "performance"));
    assert!(variant
        .artifacts
        .candidate
        .iter()
        .any(|artifact| artifact.target == "files/browser/screenshot.png"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn can_run_visual_compare_through_declared_provider() {
    let root = tmp_dir("visual-compare");
    let baseline_browser = root.join("baseline/files/browser");
    let candidate_browser = root.join("candidate/files/browser");
    fs::create_dir_all(&baseline_browser).expect("baseline browser dir");
    fs::create_dir_all(&candidate_browser).expect("candidate browser dir");
    fs::write(baseline_browser.join("screenshot.png"), "baseline image").expect("baseline image");
    fs::write(candidate_browser.join("screenshot.png"), "candidate image")
        .expect("candidate image");
    write_fixture_file(
        &root.join("baseline"),
        "summary.json",
        r#"{
            "schema":"custom-provider/browser-probe/v1",
            "summary": { "assertions": {"total":1,"passed":1,"failed":0,"skipped":0}, "networkEvents": 1 },
            "files": { "screenshot": "files/browser/screenshot.png" }
        }"#,
    );
    write_fixture_file(
        &root.join("candidate"),
        "summary.json",
        r#"{
            "schema":"custom-provider/browser-probe/v1",
            "summary": { "assertions": {"total":1,"passed":1,"failed":0,"skipped":0}, "networkEvents": 1 },
            "files": { "screenshot": "files/browser/screenshot.png" }
        }"#,
    );
    let provider = root.join("fake-visual-provider.js");
    fs::write(
        &provider,
        r#"
const fs = require('node:fs');
const input = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
fs.writeFileSync(process.env.HOMEBOY_FAKE_VISUAL_INPUT, JSON.stringify({ input }));
process.stdout.write(JSON.stringify({
    status: 'different',
    metrics: {
      visual_mismatch_ratio: 0.125,
      visual_mismatch_pixels: 25,
      visual_total_pixels: 200,
      visual_dimension_mismatch: false
    },
    artifacts: {
      visual_source_screenshot: { path: 'visual/source.png', kind: 'png' },
      visual_candidate_screenshot: { path: 'visual/candidate.png', kind: 'png' },
      visual_diff_screenshot: { path: 'visual/diff.png', kind: 'png' },
      visual_diff_json: { path: 'visual/diff.json', kind: 'json' }
    }
  }));
"#,
    )
    .expect("provider should write");
    let input_capture = root.join("visual-input-capture.json");
    let previous_capture = std::env::var_os("HOMEBOY_FAKE_VISUAL_INPUT");
    unsafe {
        std::env::set_var("HOMEBOY_FAKE_VISUAL_INPUT", &input_capture);
    }

    let report = browser_evidence_compare_from_dirs_with_visual_and_adapters(
        &[root.join("baseline")],
        &[root.join("candidate")],
        "baseline-main",
        "candidate-pr",
        false,
        Some(VisualCompareOptions {
            artifacts_dir: root.join("visual-output"),
            provider_command: "node".to_string(),
            provider_args: vec![provider.to_string_lossy().to_string()],
            threshold: Some(0.1),
        }),
        &[browser_evidence_adapter()],
    )
    .expect("report renders");

    let variant = report.variants.first().expect("variant should exist");
    let visual = variant.visual_compare.as_ref().expect("visual result");
    assert_eq!(visual.status.as_deref(), Some("different"));
    assert_eq!(visual.mismatch_ratio, Some(0.125));
    assert_eq!(visual.mismatch_pixels, Some(25));
    assert_eq!(visual.total_pixels, Some(200));
    assert!(visual
        .artifacts
        .iter()
        .any(|artifact| artifact.label == "visual_diff_screenshot"));
    assert!(report.markdown.contains("**Visual compare**"));
    assert!(report
        .markdown
        .contains("visual_diff_json: visual/diff.json"));

    let captured: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&input_capture).expect("captured input should exist"),
    )
    .expect("captured input should parse");
    assert_eq!(captured["input"]["threshold"], 0.1);
    assert_eq!(
        captured["input"]["schema"],
        "homeboy/visual-compare-request/v1"
    );
    assert!(captured["input"]["source_screenshot"]
        .as_str()
        .expect("source screenshot")
        .ends_with("baseline/files/browser/screenshot.png"));
    assert!(captured["input"]["candidate_screenshot"]
        .as_str()
        .expect("candidate screenshot")
        .ends_with("candidate/files/browser/screenshot.png"));

    unsafe {
        if let Some(value) = previous_capture {
            std::env::set_var("HOMEBOY_FAKE_VISUAL_INPUT", value);
        } else {
            std::env::remove_var("HOMEBOY_FAKE_VISUAL_INPUT");
        }
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn keeps_runtime_and_artifact_manifests_out_of_variant_matrix() {
    let root = tmp_dir("artifact-manifests");
    write_fixture_file(
        &root.join("baseline"),
        "browser-evidence.json",
        r#"{
            "scenario_id":"checkout-flow",
            "profile":"desktop",
            "browser_metrics":{"ready_ms":900},
            "request_summary":{"total":4},
            "artifacts":[{"label":"trace","path":"baseline/trace.zip"}]
        }"#,
    );
    write_fixture_file(
        &root.join("candidate"),
        "browser-evidence.json",
        r#"{
            "scenario_id":"checkout-flow",
            "profile":"desktop",
            "browser_metrics":{"ready_ms":1100},
            "request_summary":{"total":5},
            "artifacts":[{"label":"trace","path":"candidate/trace.zip"}]
        }"#,
    );
    for side in ["baseline", "candidate"] {
        write_fixture_file(
            &root.join(side),
            "artifact-bundle-sha256-deadbeef.json",
            r#"{
                "id":"artifact-bundle-sha256-deadbeef",
                "summary":{"sha256":"deadbeef","file_count":2},
                "files":{
                    "summary":"artifact-bundle/summary.json",
                    "trace":"artifact-bundle/trace.zip"
                }
            }"#,
        );
        write_fixture_file(
            &root.join(side),
            "runtime-reference-manifest-sha256-cafebabe.json",
            r#"{
                "id":"runtime-reference-manifest-sha256-cafebabe",
                "files":{
                    "runtime_reference_manifest":"runtime/reference-manifest.json"
                }
            }"#,
        );
    }

    let report = browser_evidence_compare_from_args(&args(&root, false)).expect("report renders");

    assert_eq!(report.totals.baseline_samples, 1);
    assert_eq!(report.totals.candidate_samples, 1);
    assert_eq!(report.totals.variant_count, 1);
    assert_eq!(report.variants[0].variant.scenario, "checkout-flow");
    assert_eq!(report.variants[0].variant.profile, "desktop");
    assert_eq!(
        report.variants[0].browser_metrics["ready_ms"].median_delta,
        Some(200.0)
    );
    assert!(report
        .markdown
        .contains("### Provenance / Artifact Records"));
    assert!(report
        .markdown
        .contains("artifact-bundle-sha256-deadbeef.json"));
    assert!(report
        .markdown
        .contains("runtime-reference-manifest-sha256-cafebabe.json"));
    assert!(!report
        .markdown
        .contains("`artifact-bundle-sha256-deadbeef`"));
    assert!(!report
        .markdown
        .contains("`runtime-reference-manifest-sha256-cafebabe`"));

    let _ = fs::remove_dir_all(&root);
}
