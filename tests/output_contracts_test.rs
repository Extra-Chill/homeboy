use std::collections::{BTreeMap, BTreeSet};

use homeboy::cli_surface::current_command_surface;
use homeboy::command_contract::{
    registered_command_json_family, CommandJsonFamily, PUBLIC_OUTPUT_VARIANT_CONTRACTS,
};
use homeboy::commands::bench::BenchOutput;
use homeboy::commands::extension::{ExtensionDetail, ExtensionOutput};
use homeboy::commands::rig::RigCommandOutput;
use homeboy::commands::runs::RunsOutput;
use homeboy::core::extension::StructuredSidecarDeclaration;
use serde::Serialize;
use serde_json::{json, Value};

const REQUIRED_QUALITY_COMMANDS: &[&str] = &["audit", "lint", "review", "test"];
const REQUIRED_OPS_VARIANT_COMMANDS: &[&str] = &["db", "deploy"];

struct VariantContract {
    name: &'static str,
    value: Value,
}

#[test]
fn visible_quality_commands_stay_on_quality_json_family() {
    let surface = current_command_surface();

    for command in REQUIRED_QUALITY_COMMANDS {
        assert!(
            surface.contains_path(&[*command]),
            "required quality command is not visible in the CLI surface: {command}"
        );
        assert_eq!(
            registered_command_json_family(command),
            Some(CommandJsonFamily::Quality),
            "quality command should stay routed through the quality JSON family: {command}"
        );
    }
}

#[test]
fn public_output_variant_contracts_cover_known_ops_command_families() {
    let surface = current_command_surface();
    let covered: BTreeSet<_> = PUBLIC_OUTPUT_VARIANT_CONTRACTS
        .iter()
        .map(|contract| contract.command)
        .collect();

    for command in REQUIRED_OPS_VARIANT_COMMANDS {
        assert!(
            surface.contains_path(&[*command]),
            "required ops output variant command is not visible in the CLI surface: {command}"
        );
        assert_eq!(
            registered_command_json_family(command),
            Some(CommandJsonFamily::Ops),
            "required output variant command should stay routed through the ops JSON family: {command}"
        );
        assert!(
            covered.contains(command),
            "missing public output variant contract for ops command family: {command}"
        );
    }
}

#[test]
fn runs_rig_and_bench_output_variants_have_unambiguous_contracts() {
    assert!(
        [
            std::any::type_name::<RunsOutput>(),
            std::any::type_name::<RigCommandOutput>(),
            std::any::type_name::<BenchOutput>(),
        ]
        .iter()
        .all(|output_type| output_type.starts_with("homeboy::commands::")),
        "contract test should stay anchored to public command output enums"
    );

    assert_unique_variant_signatures(
        "runs",
        vec![
            variant_contract("list", json!({ "command": "runs.list", "runs": [] })),
            variant_contract(
                "distribution",
                json!({ "command": "runs.distribution", "filters": {}, "fields": [] }),
            ),
            variant_contract(
                "latest_run",
                json!({ "command": "runs.latest-run", "run": {} }),
            ),
            variant_contract("compare", json!({ "command": "runs.compare", "rows": [] })),
            variant_contract("show", json!({ "command": "runs.show", "run": {} })),
            variant_contract(
                "evidence",
                json!({
                    "command": "runs.evidence",
                    "run_id": "run-1",
                    "run": {},
                    "metadata": {},
                    "heartbeat": {},
                    "artifact_index": {},
                    "retention": {},
                    "failure": {},
                    "disk_budget": {},
                    "evidence_links": []
                }),
            ),
            variant_contract(
                "artifacts",
                json!({ "command": "runs.artifacts", "run_id": "run-1", "artifacts": [] }),
            ),
            variant_contract(
                "artifact_get",
                json!({
                    "command": "runs.artifact.get",
                    "run_id": "run-1",
                    "artifact_id": "summary",
                    "output_path": "summary.json"
                }),
            ),
            variant_contract(
                "artifact_preview",
                json!({
                    "command": "runs.artifact.preview",
                    "run_id": "run-1",
                    "artifact_id": "generated-site",
                    "artifact_path": "/tmp/site",
                    "base_url": "http://127.0.0.1:8080/",
                    "process_id": 1234,
                    "entrypoints": [],
                    "stop_hint": "Stop preview server with `kill 1234`."
                }),
            ),
            variant_contract(
                "artifact_cleanup_downloads",
                json!({
                    "command": "runs.artifact.cleanup-downloads",
                    "dry_run": true,
                    "root": "/tmp/homeboy",
                    "removed": false,
                    "file_count": 0,
                    "directory_count": 0,
                    "size_bytes": 0,
                    "paths": []
                }),
            ),
            variant_contract(
                "artifact_cleanup_persisted",
                json!({
                    "command": "runs.artifact.cleanup-persisted",
                    "dry_run": true,
                    "artifact_root": "/tmp/homeboy/artifacts",
                    "older_than_days": 30,
                    "inspected_count": 0,
                    "planned_record_count": 0,
                    "planned_file_count": 0,
                    "planned_directory_count": 0,
                    "planned_size_bytes": 0,
                    "removed_record_count": 0,
                    "removed_file_count": 0,
                    "removed_directory_count": 0,
                    "removed_size_bytes": 0,
                    "skipped_count": 0,
                    "rows": []
                }),
            ),
            variant_contract(
                "findings",
                json!({ "command": "runs.findings", "findings": [] }),
            ),
            variant_contract(
                "finding",
                json!({ "command": "runs.finding", "finding": {} }),
            ),
            variant_contract(
                "latest_finding",
                json!({ "command": "runs.latest-finding", "run": {}, "finding": {} }),
            ),
            variant_contract(
                "bench_compare",
                json!({
                    "command": "runs.bench-compare",
                    "from_run": {},
                    "to_run": {},
                    "comparisons": [],
                    "missing": []
                }),
            ),
            variant_contract(
                "reconcile",
                json!({ "command": "runs.reconcile", "stale_runs": [] }),
            ),
            variant_contract(
                "export",
                json!({ "command": "runs.export", "output": "bundle", "manifest": {} }),
            ),
            variant_contract(
                "import",
                json!({ "command": "runs.import", "input": "bundle", "imported": {} }),
            ),
            variant_contract(
                "import_from_gh_actions",
                json!({ "command": "runs.import-gh-actions", "imported": {} }),
            ),
            variant_contract(
                "query",
                json!({
                    "command": "runs.query",
                    "filters": {},
                    "select": [],
                    "matched_artifact_count": 0,
                    "skipped_artifact_count": 0
                }),
            ),
            variant_contract(
                "drift",
                json!({
                    "command": "runs.drift",
                    "filters": {},
                    "metric": "$.status",
                    "threshold": 0.5,
                    "window_observations": 0,
                    "window_missing_rows": 0,
                    "values": []
                }),
            ),
            variant_contract(
                "loop_sync",
                json!({
                    "command": "runs.loop-sync",
                    "dry_run": true,
                    "archive_root": "/tmp/loop-archives",
                    "run_id": null,
                    "synced_artifacts": [],
                    "triage": {}
                }),
            ),
        ],
    );

    assert_unique_variant_signatures(
        "rig",
        vec![
            variant_contract("list", json!({ "command": "rig.list", "rigs": [] })),
            variant_contract("show", json!({ "command": "rig.show", "rig": {} })),
            variant_contract("up", json!({ "command": "rig.up", "steps": [] })),
            variant_contract("check", json!({ "command": "rig.check", "checks": [] })),
            variant_contract("down", json!({ "command": "rig.down", "steps": [] })),
            variant_contract("repair", json!({ "command": "rig.repair", "steps": [] })),
            variant_contract("sync", json!({ "command": "rig.sync", "stacks": [] })),
            variant_contract("status", json!({ "command": "rig.status", "rigs": [] })),
            variant_contract(
                "install",
                json!({
                    "command": "rig.install",
                    "source": "fixtures",
                    "package_path": ".",
                    "linked": false,
                    "installed": [],
                    "installed_stacks": []
                }),
            ),
            variant_contract("update", json!({ "command": "rig.update", "updated": [] })),
            variant_contract(
                "sources_list",
                json!({ "command": "rig.sources.list", "sources": [] }),
            ),
            variant_contract(
                "sources_remove",
                json!({ "command": "rig.sources.remove", "removed": true }),
            ),
            variant_contract(
                "app_install",
                json!({ "command": "rig.app.install", "apps": [] }),
            ),
            variant_contract("runs", json!({ "command": "runs.list", "runs": [] })),
        ],
    );

    assert_unique_variant_signatures(
        "bench",
        vec![
            variant_contract(
                "single",
                json!({
                    "passed": true,
                    "status": "passed",
                    "component": "homeboy",
                    "exit_code": 0,
                    "iterations": 10
                }),
            ),
            variant_contract(
                "comparison",
                json!({
                    "comparison": "cross_rig",
                    "passed": true,
                    "component": "homeboy",
                    "exit_code": 0,
                    "iterations": 10,
                    "rigs": [],
                    "diff": {},
                    "reports": {}
                }),
            ),
            variant_contract(
                "comparison_summary",
                json!({
                    "comparison": "cross_rig",
                    "summary_only": true,
                    "passed": true,
                    "component": "homeboy",
                    "exit_code": 0,
                    "iterations": 10,
                    "rigs": []
                }),
            ),
            variant_contract(
                "list",
                json!({
                    "component": "homeboy",
                    "component_id": "homeboy",
                    "scenarios": [],
                    "count": 0
                }),
            ),
            variant_contract("observation", json!({ "command": "runs.list", "runs": [] })),
        ],
    );
}

#[test]
fn extension_show_output_contracts_use_top_level_structured_sidecars() {
    let output = typed_output_value(ExtensionOutput::Show {
        extension: ExtensionDetail {
            id: "sample-extension".to_string(),
            name: "Sample Extension".to_string(),
            version: "1.0.0".to_string(),
            description: None,
            author: None,
            homepage: None,
            source_url: None,
            runtime: "platform".to_string(),
            runtime_requirements: None,
            has_setup: None,
            has_ready_check: None,
            ready: true,
            ready_reason: None,
            ready_detail: None,
            linked: false,
            path: "/extensions/sample-extension".to_string(),
            source_revision: None,
            cli: None,
            actions: Vec::new(),
            inputs: Vec::new(),
            settings: Vec::new(),
            structured_sidecars: vec![StructuredSidecarDeclaration {
                name: "findings".to_string(),
                path: "findings.json".to_string(),
                schema_version: Some("1".to_string()),
                producer: Some("lint".to_string()),
            }],
            materialization_source: None,
            requires: None,
        },
    });

    assert_eq!(
        output["extension"]["structured_sidecars"],
        json!([{ "name": "findings", "path": "findings.json", "schema_version": "1", "producer": "lint" }])
    );
    assert_eq!(output["extension"].get("lint"), None);
}

fn typed_output_value<T: Serialize>(output: T) -> Value {
    serde_json::to_value(output).expect("command output should serialize")
}

fn variant_contract(name: &'static str, value: Value) -> VariantContract {
    VariantContract { name, value }
}

fn assert_unique_variant_signatures(group: &str, contracts: Vec<VariantContract>) {
    let mut signatures = BTreeMap::<String, &'static str>::new();

    for contract in contracts {
        let signature = variant_signature(&contract.value);
        if let Some(existing) = signatures.insert(signature.clone(), contract.name) {
            panic!(
                "{group} output variants `{existing}` and `{}` share ambiguous signature `{signature}`",
                contract.name
            );
        }
    }
}

fn variant_signature(value: &Value) -> String {
    if let Some(command) = value.get("command").and_then(Value::as_str) {
        return format!("command={command}");
    }

    if let Some(comparison) = value.get("comparison").and_then(Value::as_str) {
        return if value
            .get("summary_only")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            format!("comparison={comparison};summary_only=true")
        } else {
            format!("comparison={comparison};summary_only=false")
        };
    }

    let keys = value
        .as_object()
        .expect("variant contract payload should be a JSON object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");

    format!("shape={keys}")
}
