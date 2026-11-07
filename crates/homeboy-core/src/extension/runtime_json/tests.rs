use super::bench::{self, BenchResultsRequest};
use super::sidecar;
use super::test_failure::{self, TestFailureInput};
use super::Json;

fn path_str(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn temp_entries(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".homeboy-sidecar-"))
        .collect()
}

#[test]
fn ordered_json_preserves_key_order_and_compacts_utf8() {
    let value =
        Json::parse("{ \"z\": 1, \"a\": [true, null, 1.0, \"é\\u00e9\"], \"z\": 2 }").unwrap();
    assert_eq!(value.to_compact(), r#"{"z":2,"a":[true,null,1.0,"éé"]}"#);
    assert_eq!(
        value.to_compact_ascii(),
        r#"{"z":2,"a":[true,null,1.0,"\u00e9\u00e9"]}"#
    );
    assert_eq!(
        Json::string("😀\u{7f}\n").to_compact_ascii(),
        r#""\ud83d\ude00\u007f\n""#
    );
    assert_eq!(
        Json::parse(r#"{"a":{},"b":[1]}"#)
            .unwrap()
            .to_pretty_ascii(),
        "{\n  \"a\": {},\n  \"b\": [\n    1\n  ]\n}"
    );
}

#[test]
fn sidecar_write_append_and_merge_keep_compact_shape() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nested").join("findings.json");
    let target_str = path_str(&target);
    let source = dir.path().join("extra.json");
    std::fs::write(&source, r#"[ {"tool":"lint", "message":"three"} ]"#).unwrap();

    sidecar::write_array(
        &target_str,
        &[r#"{"tool":"lint","message":"one"}"#.to_string()],
    )
    .unwrap();
    sidecar::append(&target_str, r#"{ "tool": "lint", "message": "twö" }"#).unwrap();
    sidecar::merge(&target_str, &path_str(&source)).unwrap();

    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "[{\"tool\":\"lint\",\"message\":\"one\"},{\"tool\":\"lint\",\"message\":\"twö\"},{\"tool\":\"lint\",\"message\":\"three\"}]\n"
    );
    assert!(temp_entries(target.parent().unwrap()).is_empty());
}

#[test]
fn sidecar_append_creates_missing_and_empty_targets() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("failures.json");
    std::fs::write(&target, "").unwrap();
    sidecar::append(&path_str(&target), r#"{"test_id":"a"}"#).unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "[{\"test_id\":\"a\"}]\n"
    );

    let missing = dir.path().join("missing.json");
    sidecar::append(&path_str(&missing), "1").unwrap();
    assert_eq!(std::fs::read_to_string(&missing).unwrap(), "[1]\n");
}

#[test]
fn sidecar_write_object_requires_object() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("results.json");
    sidecar::write_object(&path_str(&target), r#"{"total":5,"passed":3}"#).unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "{\"total\":5,\"passed\":3}\n"
    );
    let error = sidecar::write_object(&path_str(&target), "[1]").unwrap_err();
    assert!(error.contains("JSON object"), "{error}");
}

#[test]
fn sidecar_failures_leave_existing_target_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("findings.json");
    let target_str = path_str(&target);
    std::fs::write(&target, "{\"not\":\"array\"}\n").unwrap();

    let error = sidecar::append(&target_str, "{}").unwrap_err();
    assert!(error.contains("must contain a JSON array"), "{error}");
    assert!(sidecar::write_array(&target_str, &["{broken".to_string()]).is_err());
    let source = dir.path().join("source.json");
    std::fs::write(&source, "{}").unwrap();
    assert!(sidecar::merge(&target_str, &path_str(&source)).is_err());

    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "{\"not\":\"array\"}\n"
    );
    assert!(temp_entries(dir.path()).is_empty());
}

#[test]
fn sidecar_empty_target_or_source_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t.json");
    sidecar::write_array("", &["{".to_string()]).unwrap();
    sidecar::append("", "{").unwrap();
    sidecar::write_object("", "[").unwrap();
    sidecar::merge(&path_str(&target), "").unwrap();
    sidecar::merge(
        &path_str(&target),
        &path_str(&dir.path().join("absent.json")),
    )
    .unwrap();
    assert!(!target.exists());
}

#[test]
fn test_failure_record_has_fixed_shape_and_canonical_fingerprint() {
    let record = test_failure::record(&TestFailureInput {
        test_id: "Foo::testBar".into(),
        suite: "phpunit".into(),
        file: "src/Foo.php".into(),
        line: "42".into(),
        message: "Failed asserting.\nmore".into(),
        failure_type: "AssertionError".into(),
        stdout_excerpt: "out".into(),
        ..Default::default()
    });
    let identity = "Foo::testBar\0src/Foo.php\042\0AssertionError\0Failed asserting.";
    let fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(identity.as_bytes());
    assert_eq!(
        record.to_compact(),
        format!(
            r#"{{"test_id":"Foo::testBar","suite":"phpunit","file":"src/Foo.php","line":42,"message":"Failed asserting.\nmore","failure_type":"AssertionError","fingerprint":"{fingerprint}","stdout_excerpt":"out","stderr_excerpt":""}}"#
        )
    );

    let infra = test_failure::record(&TestFailureInput {
        test_id: "cargo test".into(),
        line: "x".into(),
        identity: Some("rust:cargo-test:failed".into()),
        ..Default::default()
    });
    assert_eq!(infra.get("suite"), Some(&Json::Null));
    assert_eq!(infra.get("line"), Some(&Json::Null));
    assert_eq!(
        infra.get("failure_type"),
        Some(&Json::string("test_failure"))
    );
    assert_eq!(
        infra.get("fingerprint"),
        Some(&Json::String(
            homeboy_engine_primitives::content_hash::sha256_hex(b"rust:cargo-test:failed")
        ))
    );
}

#[test]
fn bound_excerpt_keeps_forty_lines_and_caps_characters() {
    let raw = (0..50)
        .map(|i| format!("{i}-{}", "x".repeat(196)))
        .collect::<Vec<_>>()
        .join("\n");
    let excerpt = test_failure::bound_excerpt(&raw);
    assert_eq!(excerpt.chars().count(), 4000);
    assert!(excerpt.ends_with("..."));

    let short = (0..45)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        test_failure::bound_excerpt(&format!("\n  {short}")),
        (0..39)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn percentile_r7_interpolates() {
    assert_eq!(bench::percentile_r7(&[], 0.5), 0.0);
    assert_eq!(bench::percentile_r7(&[4.0], 0.99), 4.0);
    assert_eq!(bench::percentile_r7(&[1.0, 3.0, 5.0], 0.5), 3.0);
    assert!((bench::percentile_r7(&[1.0, 3.0, 5.0], 0.95) - 4.8).abs() < 1e-12);
}

#[test]
fn bench_results_envelope_matches_runner_contract() {
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload.json");
    std::fs::write(
        &payload,
        r#"{"timings_ns":[5000000,1000000,3000000],"peak_rss_bytes":4096,"source":"custom","metadata":{"fixture":true},"metrics":{"custom_ms":9.5,"note":"skip"}}"#,
    )
    .unwrap();
    let empty = dir.path().join("empty.json");
    std::fs::write(&empty, r#"{"timings_ns":[]}"#).unwrap();
    let results = dir.path().join("out").join("bench-results.json");

    let warnings = bench::write_results(&BenchResultsRequest {
        results_file: path_str(&results),
        component_id: "demo".into(),
        iterations: 3,
        metadata_json: r#"{"runner":"ok"}"#.into(),
        records: vec![
            format!("bench-fast={}", payload.display()),
            format!("bench-empty={}", empty.display()),
        ],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        warnings,
        vec!["WORKLOAD_WARN: bench-empty emitted no timings"]
    );
    assert_eq!(
        std::fs::read_to_string(&results).unwrap(),
        r#"{
  "component_id": "demo",
  "iterations": 3,
  "scenarios": [
    {
      "id": "bench-fast",
      "iterations": 3,
      "metrics": {
        "mean_ms": 3.0,
        "p50_ms": 3.0,
        "p95_ms": 4.800000000000001,
        "p99_ms": 4.960000000000001,
        "min_ms": 1.0,
        "max_ms": 5.0,
        "custom_ms": 9.5
      },
      "source": "custom",
      "metadata": {
        "fixture": true
      },
      "memory": {
        "peak_bytes": 4096
      }
    }
  ],
  "metadata": {
    "runner": "ok"
  }
}
"#
    );
}

#[test]
fn bench_inventory_accepts_payloadless_records() {
    let (envelope, warnings) = bench::results_envelope(&BenchResultsRequest {
        inventory: true,
        component_id: "demo".into(),
        iterations: 11,
        records: vec!["bench-http=src/bin/bench-http.rs=native-bin".into()],
        ..Default::default()
    })
    .unwrap();
    assert!(warnings.is_empty());
    assert_eq!(
        envelope.to_compact(),
        r#"{"component_id":"demo","iterations":0,"scenarios":[{"id":"bench-http","iterations":0,"default_iterations":11,"tags":[],"metrics":{},"file":"src/bin/bench-http.rs","source":"native-bin"}]}"#
    );
}

#[test]
fn bench_artifact_ref_and_ping_shapes() {
    assert_eq!(
        bench::artifact_ref("bench.log", "text", "Bénch").to_compact_ascii(),
        r#"{"path":"bench.log","kind":"text","label":"B\u00e9nch"}"#
    );
    assert_eq!(
        bench::artifact_ref("bench.log", "", "").to_compact_ascii(),
        r#"{"path":"bench.log"}"#
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nested").join("pings.jsonl");
    let started = bench::now_ms();
    bench::responsiveness_ping(&path_str(&file), started, "warm").unwrap();
    bench::responsiveness_ping(&path_str(&file), started + 60_000, "").unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["label"], "warm");
    assert!(first["at"].as_str().unwrap().ends_with('Z'));
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["t_ms"], 0, "t_ms clamps at zero");
    assert!(second.get("label").is_none());
}
