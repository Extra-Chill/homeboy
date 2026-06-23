use std::collections::BTreeMap;

use serde_json::json;

use crate::core::fuzz::*;

#[test]
fn parse_fuzz_case_log_contents_accepts_jsonl_entries() {
    let entries = parse_fuzz_case_log_contents(
        r#"{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","operation_family":"read","seed":"seed-1","input_hash":"sha256:abc","status":"passed","duration_ms":12,"artifact_refs":[{"id":"artifact-1","kind":"stdout","path":"logs/case-1.txt"}]}
{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-2","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:def","status":"skipped","duration_ms":1,"skip_reason":"auth_required"}"#,
    )
    .expect("parse case log jsonl");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].schema, FUZZ_CASE_LOG_SCHEMA);
    assert_eq!(entries[0].operation_family, Some(FuzzOperationFamily::Read));
    assert_eq!(entries[0].artifact_refs[0].kind, "stdout");
    assert_eq!(entries[1].status, FuzzCaseLogStatus::Skipped);
    assert_eq!(entries[1].skip_reason.as_deref(), Some("auth_required"));
}

#[test]
fn parse_fuzz_case_log_contents_rejects_invalid_entries() {
    let err = parse_fuzz_case_log_contents(
        r#"[{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:abc","status":"failed","duration_ms":4}]"#,
    )
    .expect_err("failed entry without reason should fail");

    assert!(err.contains("failure_fingerprint or failure_reason"));

    let err = parse_fuzz_case_log_contents(
        r#"{"entries":[{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:abc","status":"passed"}]}"#,
    )
    .expect_err("entry without timing should fail");

    assert!(err.contains("duration_ms or timestamps"));
}

#[test]
fn parse_fuzz_results_file_reads_campaign_contract() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("fuzz-results.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "schema": FUZZ_CAMPAIGN_SCHEMA,
            "id": "campaign-1",
            "safety_class": "read_only"
        })
        .to_string(),
    )
    .expect("write fuzz results");

    let parsed = parse_fuzz_results_file(&path).expect("parse fuzz results");

    assert_eq!(parsed.id, "campaign-1");
    assert_eq!(parsed.safety_class, FuzzSafetyClass::ReadOnly);
}

#[test]
fn parse_fuzz_target_inventory_file_reads_and_normalizes_inventory_contract() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("fuzz-inventory.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "schema": FUZZ_TARGET_INVENTORY_SCHEMA,
            "id": " discovered ",
            "surfaces": [{
                "id": " api ",
                "kind": " rest ",
                "safety_class": "read_only"
            }],
            "targets": [{
                "id": " target-1 ",
                "kind": " endpoint "
            }],
            "workloads": [{
                "id": " workload-1 ",
                "safety_class": "read_only",
                "surface_ids": [" api ", " "]
            }],
            "seeds": [{
                "id": " seed-1 ",
                "kind": " literal ",
                "tags": [" stable ", " "]
            }]
        })
        .to_string(),
    )
    .expect("write fuzz inventory");

    let parsed = parse_fuzz_target_inventory_file(&path).expect("parse fuzz inventory");

    assert_eq!(parsed.id, "discovered");
    assert_eq!(parsed.surfaces[0].id, "api");
    assert_eq!(parsed.targets[0].kind, "endpoint");
    assert_eq!(parsed.workloads[0].surface_ids, vec!["api"]);
    assert_eq!(parsed.seeds[0].tags, vec!["stable"]);
}

#[test]
fn merge_fuzz_target_inventory_appends_discovered_contract_sections() {
    let mut base = FuzzTargetInventory {
        schema: FUZZ_TARGET_INVENTORY_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: "base".to_string(),
        surfaces: Vec::new(),
        targets: Vec::new(),
        workloads: Vec::new(),
        seeds: Vec::new(),
        provenance: None,
        metadata: json!({ "declared_workloads": [] }),
        extra: BTreeMap::new(),
    };
    let discovered = FuzzTargetInventory::from_value(json!({
        "schema": FUZZ_TARGET_INVENTORY_SCHEMA,
        "id": "discovered",
        "surfaces": [{
            "id": "api",
            "kind": "rest",
            "safety_class": "read_only"
        }],
        "metadata": { "producer": "runner" }
    }))
    .expect("inventory contract");

    merge_fuzz_target_inventory(&mut base, discovered);

    assert_eq!(base.id, "base");
    assert_eq!(base.surfaces.len(), 1);
    assert_eq!(base.metadata["declared_workloads"], json!([]));
    assert_eq!(base.metadata["producer"], "runner");
}
