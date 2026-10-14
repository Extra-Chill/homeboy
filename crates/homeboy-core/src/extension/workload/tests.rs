use std::path::{Path, PathBuf};

use homeboy_engine_primitives::baseline::BaselineFlags;
use homeboy_engine_primitives::workload_compare::{
    apply_baseline_flags, compare, load_baseline, BaselineSlot, KindAdapter,
};
use homeboy_extension_contract::{
    WorkloadCompareConfig, WorkloadResult, WorkloadSafetyClass, WORKLOAD_RESULT_SCHEMA,
};
use serde_json::{json, Value};

use super::case::{self, CaseMode, CaseRequest};
use super::compare::{MetricChange, WorkloadAdapter, WorkloadComparison};
use super::list;
use super::resolve::{resolve_kind, WorkloadTarget};
use super::run::{self as workload_run, outcome_status, read_envelope, WorkloadRunRequest};
use super::safety::{self, DestructiveRequest};
use super::stored;
use super::{
    baseline_key, env, selected_rig_workload, RigWorkloadFile, WORKLOAD_ARTIFACT_KIND,
    WORKLOAD_RESULT_ARTIFACT_KIND,
};
use crate::observation::{ObservationStore, RunStatus};

fn result(value: Value) -> WorkloadResult {
    serde_json::from_value(value).expect("envelope")
}

fn envelope(status: &str, metrics: Value, findings: &[&str], cases: Value) -> WorkloadResult {
    result(json!({
        "schema": WORKLOAD_RESULT_SCHEMA,
        "kind": "demo",
        "workload_id": "w",
        "status": status,
        "metrics": metrics,
        "findings": findings.iter().map(|fingerprint| json!({
            "tool": "demo",
            "message": format!("finding {fingerprint}"),
            "fingerprint": fingerprint,
        })).collect::<Vec<_>>(),
        "cases": cases,
    }))
}

fn config(value: Value) -> WorkloadCompareConfig {
    serde_json::from_value(value).expect("compare config")
}

fn latency_config() -> WorkloadCompareConfig {
    config(json!({ "metrics": [
        { "name": "p95_ms", "direction": "lower_is_better", "regression_threshold": 10.0 },
        { "name": "coverage", "direction": "higher_is_better" }
    ] }))
}

// --- generic adapter -------------------------------------------------------

#[test]
fn declared_metrics_gate_by_direction_and_threshold() {
    let base = envelope(
        "pass",
        json!({ "p95_ms": 100, "coverage": 50, "noise": 1 }),
        &[],
        json!([]),
    );
    let within = envelope(
        "pass",
        json!({ "p95_ms": 109, "coverage": 50, "noise": 9 }),
        &[],
        json!([]),
    );
    let worse = envelope(
        "pass",
        json!({ "p95_ms": 120, "coverage": 49 }),
        &[],
        json!([]),
    );
    let better = envelope(
        "pass",
        json!({ "p95_ms": 80, "coverage": 51, "noise": 1 }),
        &[],
        json!([]),
    );

    let same = compare::<WorkloadAdapter>(&base, &within, &latency_config()).unwrap();
    assert_eq!(same.verdict.status(), "same", "{:?}", same.verdict);
    let noise = same
        .delta
        .metrics
        .iter()
        .find(|m| m.name == "noise")
        .unwrap();
    assert!(!noise.gated, "undeclared metrics never gate");
    assert_eq!(noise.delta, Some(8.0));

    let regressed = compare::<WorkloadAdapter>(&base, &worse, &latency_config()).unwrap();
    assert_eq!(regressed.verdict.status(), "worse");
    assert_eq!(
        regressed.verdict.regressions,
        vec![
            "metric `coverage` regressed".to_string(),
            "metric `p95_ms` regressed".to_string()
        ]
    );
    let p95 = regressed
        .delta
        .metrics
        .iter()
        .find(|m| m.name == "p95_ms")
        .unwrap();
    assert_eq!(p95.change, MetricChange::Regressed);
    assert_eq!(p95.percent, Some(20.0));

    let improved = compare::<WorkloadAdapter>(&base, &better, &latency_config()).unwrap();
    assert_eq!(improved.verdict.status(), "better");
    assert_eq!(improved.verdict.improvements.len(), 2);
}

#[test]
fn a_declared_metric_that_disappears_is_a_regression() {
    let base = envelope("pass", json!({ "p95_ms": 100 }), &[], json!([]));
    let candidate = envelope("pass", json!({}), &[], json!([]));
    let compared = compare::<WorkloadAdapter>(&base, &candidate, &latency_config()).unwrap();
    assert_eq!(
        compared.verdict.regressions,
        vec!["metric `p95_ms` is no longer reported".to_string()]
    );
}

#[test]
fn findings_compare_by_fingerprint_and_status_and_cases_are_judged() {
    let base = envelope(
        "pass",
        json!({}),
        &["a", "b"],
        json!([{ "case_id": "c1", "status": "pass" }, { "case_id": "c2", "status": "fail" }]),
    );
    let candidate = envelope(
        "fail",
        json!({}),
        &["b", "c"],
        json!([{ "case_id": "c1", "status": "fail" }, { "case_id": "c3", "status": "pass" }]),
    );
    let compared = compare::<WorkloadAdapter>(&base, &candidate, &config(json!({}))).unwrap();

    assert_eq!(compared.delta.findings.new, vec!["c".to_string()]);
    assert_eq!(compared.delta.findings.resolved, vec!["a".to_string()]);
    assert_eq!(compared.delta.findings.persisting, 1);
    assert_eq!(compared.delta.cases.newly_failing, vec!["c1".to_string()]);
    assert_eq!(compared.delta.cases.added, vec!["c3".to_string()]);
    assert_eq!(compared.delta.cases.removed, vec!["c2".to_string()]);
    assert_eq!(
        compared.verdict.regressions,
        vec![
            "new finding `c`".to_string(),
            "status went from pass to fail".to_string()
        ]
    );
    assert_eq!(
        compared.verdict.advisories,
        vec!["case `c1` newly fails".to_string()]
    );
    assert_eq!(
        compared.verdict.improvements,
        vec!["resolved finding `a`".to_string()]
    );
    assert_eq!(
        WorkloadAdapter::fingerprints(&compared.candidate),
        vec!["b".to_string(), "c".to_string()]
    );
}

#[test]
fn runs_of_different_kinds_or_workloads_are_not_comparable() {
    let base = envelope("pass", json!({}), &[], json!([]));
    let mut other_kind = base.clone();
    other_kind.kind = "bench".to_string();
    let mut other_workload = base.clone();
    other_workload.workload_id = Some("x".to_string());

    let config = config(json!({}));
    assert!(compare::<WorkloadAdapter>(&base, &other_kind, &config).is_err());
    assert!(compare::<WorkloadAdapter>(&base, &other_workload, &config).is_err());
}

#[test]
fn baseline_gate_uses_the_namespaced_kind_slot() {
    let dir = tempfile::tempdir().unwrap();
    let key = baseline_key("demo");
    let slot = BaselineSlot::new(dir.path(), None).with_kind(&key);
    let flags = |baseline, ignore_baseline, ratchet| BaselineFlags {
        baseline,
        ignore_baseline,
        ratchet,
    };
    let run = |flags: &BaselineFlags, p95: u64| {
        let source = envelope("pass", json!({ "p95_ms": p95 }), &[], json!([]));
        apply_baseline_flags::<WorkloadAdapter>(
            &slot,
            "c",
            flags,
            Some(&source),
            true,
            &latency_config(),
        )
        .unwrap()
    };

    run(&flags(true, false, false), 100);
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("homeboy.json")).unwrap())
            .unwrap();
    assert!(stored["baselines"]["workload.demo"].is_object());

    let gate = run(&flags(false, false, false), 150);
    assert_eq!(gate.exit_override, Some(1));
    assert!(WorkloadComparison::new(gate.comparison.unwrap()).regressed());

    run(&flags(false, false, true), 50);
    let baseline = load_baseline::<WorkloadAdapter>(&slot).unwrap();
    assert_eq!(baseline.metadata.metrics["p95_ms"], 50.0);

    let ignored = run(&flags(false, true, false), 999);
    assert!(ignored.comparison.is_none() && ignored.exit_override.is_none());
}

// --- status, envelope, safety ---------------------------------------------

#[test]
fn outcome_status_trusts_the_envelope_only_when_the_runner_agrees() {
    let pass = envelope("pass", json!({}), &[], json!([]));
    let fail = envelope("fail", json!({}), &[], json!([]));
    let mut error = None;

    assert_eq!(
        outcome_status(true, 0, false, Some(&pass), &mut error),
        (RunStatus::Pass, 0)
    );
    assert_eq!(
        outcome_status(false, 1, false, Some(&fail), &mut error),
        (RunStatus::Fail, 1)
    );
    assert!(error.is_none());
    assert_eq!(
        outcome_status(false, 3, false, Some(&pass), &mut error),
        (RunStatus::Error, 3)
    );
    assert!(error.take().unwrap().contains("reports pass"));
    assert_eq!(
        outcome_status(true, 0, false, None, &mut error),
        (RunStatus::Error, 1)
    );
    assert_eq!(
        outcome_status(true, 0, true, Some(&pass), &mut error),
        (RunStatus::Error, 1)
    );
    assert_eq!(error.as_deref(), Some("runner timed out"));
}

fn write_envelope(dir: &Path, value: Value) -> std::path::PathBuf {
    let path = dir.join("result.json");
    std::fs::write(&path, value.to_string()).unwrap();
    path
}

#[test]
fn envelope_reader_checks_workload_artifacts_and_case_refs() {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir_all(artifacts.join("cases")).unwrap();
    std::fs::write(artifacts.join("cases/c1.json"), "{}").unwrap();
    let mut value = json!({
        "schema": WORKLOAD_RESULT_SCHEMA,
        "kind": "demo",
        "status": "fail",
        "cases": [
            { "case_id": "c1", "artifact_ref": "c1-input", "status": "fail" },
            { "case_id": "c2", "artifact_ref": "cases/c1.json", "status": "pass" }
        ],
        "artifacts": [{ "name": "c1-input", "path": "cases/c1.json" }]
    });
    let file = write_envelope(dir.path(), value.clone());
    let read = read_envelope(&file, &artifacts, "demo", None, Some("w")).unwrap();
    assert_eq!(
        read.workload_id.as_deref(),
        Some("w"),
        "requested id fills an absent one"
    );

    value["workload_id"] = json!("other");
    let file = write_envelope(dir.path(), value.clone());
    assert!(read_envelope(&file, &artifacts, "demo", None, Some("w"))
        .unwrap_err()
        .contains("requested"));

    value["workload_id"] = Value::Null;
    value["artifacts"][0]["path"] = json!("cases/missing.json");
    let file = write_envelope(dir.path(), value.clone());
    assert!(read_envelope(&file, &artifacts, "demo", None, None)
        .unwrap_err()
        .contains("did not write"));

    value["artifacts"][0]["path"] = json!("cases/c1.json");
    value["cases"][1]["artifact_ref"] = json!("nowhere");
    let file = write_envelope(dir.path(), value);
    assert!(read_envelope(&file, &artifacts, "demo", None, None)
        .unwrap_err()
        .contains("names no listed artifact"));

    assert!(read_envelope(
        &dir.path().join("absent.json"),
        &artifacts,
        "demo",
        None,
        None
    )
    .unwrap_err()
    .contains(env::RESULTS_FILE));
}

fn proof() -> Value {
    json!({
        "schema": "homeboy/isolation-proof/v1",
        "version": 1,
        "runtime_kind": "container",
        "disposable": true,
        "teardown_required": true,
        "mutation_boundary": "container filesystem",
        "proof_artifacts": [{ "kind": "container-id", "ref": "abc" }],
        "verified_by": "operator"
    })
}

#[test]
fn destructive_kinds_need_explicit_authorization_and_a_valid_proof() {
    let dir = tempfile::tempdir().unwrap();
    let proof_file = dir.path().join("proof.json");
    std::fs::write(&proof_file, proof().to_string()).unwrap();
    let request = |allow, proof: Option<&Path>| DestructiveRequest {
        allow_destructive: allow,
        isolation_proof: proof.map(Path::to_path_buf),
    };

    assert_eq!(
        safety::authorize("demo", WorkloadSafetyClass::ReadOnly, &request(false, None)).unwrap(),
        None
    );
    assert_eq!(
        safety::env(None),
        vec![(env::ALLOW_DESTRUCTIVE.to_string(), "0".to_string())]
    );

    let destructive = WorkloadSafetyClass::Destructive;
    let error =
        safety::authorize("demo", destructive, &request(false, Some(&proof_file))).unwrap_err();
    assert!(error.message.contains("--allow-destructive"));
    let error = safety::authorize("demo", destructive, &request(true, None)).unwrap_err();
    assert!(error.message.contains("--isolation-proof"));

    for (field, value) in [
        ("disposable", json!(false)),
        ("teardown_required", json!(false)),
        ("proof_artifacts", json!([])),
        ("verified_by", json!(" ")),
        ("schema", json!("homeboy/other/v1")),
    ] {
        let mut bad = proof();
        bad[field] = value;
        std::fs::write(&proof_file, bad.to_string()).unwrap();
        assert!(
            safety::authorize("demo", destructive, &request(true, Some(&proof_file))).is_err(),
            "{field} must be enforced"
        );
    }

    std::fs::write(&proof_file, proof().to_string()).unwrap();
    let authorization = safety::authorize("demo", destructive, &request(true, Some(&proof_file)))
        .unwrap()
        .expect("authorized");
    assert_eq!(
        safety::env(Some(&authorization)),
        vec![
            (env::ALLOW_DESTRUCTIVE.to_string(), "1".to_string()),
            (
                env::ISOLATION_PROOF_FILE.to_string(),
                proof_file.to_string_lossy().to_string()
            ),
        ]
    );
}

// --- end to end through a declared kind -----------------------------------

const RUNNER: &str = r#"#!/bin/sh
set -e
if [ "$HOMEBOY_WORKLOAD_LIST_ONLY" = "1" ]; then
  printf '{"workloads":[{"id":"from-script","label":"Script"}]}' > "$HOMEBOY_WORKLOAD_RESULTS_FILE"
  exit 0
fi
mkdir -p "$HOMEBOY_WORKLOAD_ARTIFACT_DIR/cases"
printf '{"input":""}' > "$HOMEBOY_WORKLOAD_ARTIFACT_DIR/cases/c2.json"
p95=$(cat "$HOMEBOY_COMPONENT_PATH/p95")
cat > "$HOMEBOY_WORKLOAD_RESULTS_FILE" <<EOF
{"schema":"homeboy/workload-result/v1","kind":"$HOMEBOY_WORKLOAD_KIND","workload_id":"$HOMEBOY_WORKLOAD_ID","status":"fail",
 "findings":[{"tool":"demo","rule":"parse","severity":"high","message":"throws on empty input","fingerprint":"throw:empty"}],
 "cases":[{"case_id":"c1","seed":7,"status":"pass"},{"case_id":"c2","seed":"$HOMEBOY_WORKLOAD_SEED","artifact_ref":"c2-input","status":"fail"}],
 "metrics":{"p95_ms":$p95,"run_id_len":${#HOMEBOY_WORKLOAD_RUN_ID}},
 "artifacts":[{"name":"c2-input","path":"cases/c2.json"}],
 "payload":{"schema":"demo/result/v1","allow_destructive":"$HOMEBOY_WORKLOAD_ALLOW_DESTRUCTIVE"}}
EOF
exit 1
"#;

const REPLAY: &str = r#"printf '%s|%s|%s|%s|%s' "$HOMEBOY_WORKLOAD_CASE_MODE" "$HOMEBOY_WORKLOAD_CASE_ID" "$HOMEBOY_WORKLOAD_SEED" "$HOMEBOY_WORKLOAD_CASE_ARTIFACT_REF" "$(cat "$HOMEBOY_WORKLOAD_CASE_ARTIFACT_FILE")""#;

fn install_fixture(home: &Path) -> std::path::PathBuf {
    let extension_dir = crate::paths::extensions()
        .expect("extensions")
        .join("kinds");
    std::fs::create_dir_all(&extension_dir).unwrap();
    std::fs::write(
        extension_dir.join("kinds.json"),
        json!({
            "name": "Kinds",
            "version": "1.0.0",
            "workloads": {
                "demo": {
                    "script": "demo.sh",
                    "workloads": [{ "id": "w", "label": "Declared" }],
                    "list_probes": [
                        { "type": "json_pointer", "path": "package.json", "pointer": "/scripts/demo", "id": "pkg" },
                        { "type": "json_pointer", "path": "package.json", "pointer": "/scripts/absent", "id": "never" },
                        { "type": "script" }
                    ],
                    "result_schema": "demo/result/v1",
                    "replay_command": REPLAY,
                    "compare": { "metrics": [{ "name": "p95_ms", "direction": "lower_is_better" }] }
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let script = extension_dir.join("demo.sh");
    std::fs::write(&script, RUNNER).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let component = home.join("component");
    std::fs::create_dir_all(&component).unwrap();
    std::fs::write(
        component.join("homeboy.json"),
        r#"{"id":"consumer","extensions":{"kinds":{}}}"#,
    )
    .unwrap();
    std::fs::write(
        component.join("package.json"),
        r#"{"scripts":{"demo":"node demo.js"}}"#,
    )
    .unwrap();
    std::fs::write(component.join("p95"), "100").unwrap();
    component
}

fn target(component: &Path) -> WorkloadTarget {
    WorkloadTarget {
        component_id: Some("consumer".to_string()),
        path_override: Some(component.to_string_lossy().to_string()),
        ..WorkloadTarget::default()
    }
}

#[test]
fn declared_kind_lists_runs_persists_compares_and_replays_end_to_end() {
    crate::test_support::with_isolated_home(|home| {
        let component = install_fixture(home.path());
        let resolved = resolve_kind("demo", target(&component)).expect("resolve kind");
        assert_eq!(resolved.extension_id(), "kinds");
        assert_eq!(resolved.extension.script_path, "demo.sh");
        assert!(resolve_kind("undeclared", target(&component)).is_err());

        let declared = list::declared_workloads(&resolved);
        assert_eq!(
            declared
                .iter()
                .map(|w| (w.id.as_str(), w.source.as_str()))
                .collect::<Vec<_>>(),
            vec![("w", "manifest"), ("pkg", "probe:json_pointer")]
        );
        assert!(list::has_script_probe(&resolved));
        let scripted = list::script_workloads(&resolved, &[], &[]).expect("script probe");
        assert_eq!(scripted[0].id, "from-script");
        assert_eq!(scripted[0].source, "probe:script");

        let execute = |run_id: &str| {
            workload_run::run(WorkloadRunRequest {
                resolved: &resolved,
                workload_id: Some("w"),
                run_id: Some(run_id),
                seed: Some("42"),
                rig_id: None,
                rig_workloads: &[],
                settings: &[],
                settings_json: &[],
                script_args: &[],
                timeout: None,
                authorization: None,
                baseline: BaselineFlags {
                    baseline: false,
                    ignore_baseline: true,
                    ratchet: false,
                },
                command: "homeboy workload run --kind demo".to_string(),
            })
            .expect("run")
        };
        let first = execute("demo-run-a");
        assert_eq!(first.status, "fail");
        assert_eq!(first.exit_code, 1);
        assert!(first.result_error.is_none(), "{:?}", first.result_error);
        let result = first.result.as_ref().unwrap();
        assert_eq!(result.case("c2").unwrap().seed.as_deref(), Some("42"));
        assert_eq!(result.payload["allow_destructive"], "0");
        assert_eq!(result.metrics["run_id_len"], "demo-run-a".len() as f64);

        let store = ObservationStore::open_initialized().unwrap();
        let run = store.get_run("demo-run-a").unwrap().unwrap();
        assert_eq!(run.kind, "demo");
        assert_eq!(run.status, "fail");
        assert_eq!(run.metadata_json["extension_id"], "kinds");
        assert_eq!(run.metadata_json["finding_count"], 1);
        assert_eq!(
            run.metadata_json["workload_compare"]["metrics"][0]["name"],
            "p95_ms"
        );
        let kinds = store
            .list_artifacts("demo-run-a")
            .unwrap()
            .into_iter()
            .map(|artifact| artifact.kind)
            .collect::<Vec<_>>();
        assert!(kinds.contains(&WORKLOAD_RESULT_ARTIFACT_KIND.to_string()));
        assert!(kinds.contains(&WORKLOAD_ARTIFACT_KIND.to_string()));
        let findings = store.list_findings_for_run("demo-run-a").unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].fingerprint.as_deref(), Some("throw:empty"));
        assert_eq!(findings[0].metadata_json["workload_kind"], "demo");
        assert_eq!(findings[0].metadata_json["workload_id"], "w");

        std::fs::write(component.join("p95"), "130").unwrap();
        execute("demo-run-b");
        let a = stored::load(&store, "demo-run-a", Some("demo")).unwrap();
        let b = stored::load(&store, "demo-run-b", Some("demo")).unwrap();
        assert!(stored::load(&store, "demo-run-a", Some("bench")).is_err());
        let compared =
            compare::<WorkloadAdapter>(&a.result, &b.result, &b.compare_config()).unwrap();
        assert_eq!(
            compared.verdict.regressions,
            vec!["metric `p95_ms` regressed".to_string()]
        );
        assert_eq!(compared.delta.findings.persisting, 1);

        let replay = |dry_run| {
            case::execute(CaseRequest {
                mode: CaseMode::Replay,
                resolved: &resolved,
                source: &a,
                case_id: "c2",
                rig_workloads: &[],
                settings: &[],
                settings_json: &[],
                script_args: &[],
                timeout: None,
                authorization: None,
                dry_run,
            })
        };
        let planned = replay(true).unwrap();
        assert_eq!(planned.status, "dry_run");
        assert!(planned
            .env
            .iter()
            .any(|item| item.name == env::CASE_ID && item.value == "c2"));
        let replayed = replay(false).unwrap();
        assert_eq!(replayed.status, "passed", "{:?}", replayed.stderr_tail);
        assert_eq!(
            replayed.stdout_tail.as_deref(),
            Some("replay|c2|42|c2-input|{\"input\":\"\"}")
        );

        let missing_case = case::execute(CaseRequest {
            mode: CaseMode::Replay,
            resolved: &resolved,
            source: &a,
            case_id: "nope",
            rig_workloads: &[],
            settings: &[],
            settings_json: &[],
            script_args: &[],
            timeout: None,
            authorization: None,
            dry_run: true,
        })
        .unwrap_err();
        assert!(missing_case.message.contains("no case `nope`"));
        let minimize = case::execute(CaseRequest {
            mode: CaseMode::Minimize,
            resolved: &resolved,
            source: &a,
            case_id: "c2",
            rig_workloads: &[],
            settings: &[],
            settings_json: &[],
            script_args: &[],
            timeout: None,
            authorization: None,
            dry_run: false,
        })
        .unwrap_err();
        assert!(minimize.message.contains("minimize_command"));
    });
}

#[test]
fn an_invalid_envelope_terminalizes_the_run_as_error_without_findings() {
    crate::test_support::with_isolated_home(|home| {
        let component = install_fixture(home.path());
        let extension_dir = crate::paths::extensions().unwrap().join("kinds");
        std::fs::write(
            extension_dir.join("demo.sh"),
            "#!/bin/sh\nprintf '{\"schema\":\"homeboy/workload-result/v1\",\"kind\":\"other\",\"status\":\"pass\"}' > \"$HOMEBOY_WORKLOAD_RESULTS_FILE\"\n",
        )
        .unwrap();
        let resolved = resolve_kind("demo", target(&component)).unwrap();
        let outcome = workload_run::run(WorkloadRunRequest {
            resolved: &resolved,
            workload_id: None,
            run_id: Some("demo-bad"),
            seed: None,
            rig_id: None,
            rig_workloads: &[],
            settings: &[],
            settings_json: &[],
            script_args: &[],
            timeout: None,
            authorization: None,
            baseline: BaselineFlags::default(),
            command: "homeboy workload run --kind demo".to_string(),
        })
        .unwrap();
        assert_eq!(outcome.status, "error");
        assert_eq!(outcome.exit_code, 1);
        assert!(outcome.result_error.unwrap().contains("does not match"));
        let store = ObservationStore::open_initialized().unwrap();
        assert_eq!(store.get_run("demo-bad").unwrap().unwrap().status, "error");
        assert!(store.list_findings_for_run("demo-bad").unwrap().is_empty());
        assert!(
            stored::load(&store, "demo-bad", None).is_err(),
            "invalid envelope is not loadable"
        );
    });
}

// --- rig workloads ----------------------------------------------------------

const PATH_RUNNER: &str = r#"#!/bin/sh
cat > "$HOMEBOY_WORKLOAD_RESULTS_FILE" <<EOF
{"schema":"homeboy/workload-result/v1","kind":"$HOMEBOY_WORKLOAD_KIND","workload_id":"$HOMEBOY_WORKLOAD_ID","status":"pass","cases":[],"metrics":{},
 "payload":{"workload_path":"$HOMEBOY_WORKLOAD_PATH","extra_paths":"$HOMEBOY_WORKLOAD_EXTRA_PATHS"}}
EOF
"#;

fn install_rig_fixture(home: &Path, safety_class: &str) -> std::path::PathBuf {
    let extension_dir = crate::paths::extensions()
        .expect("extensions")
        .join("rigkinds");
    std::fs::create_dir_all(&extension_dir).unwrap();
    std::fs::write(
        extension_dir.join("rigkinds.json"),
        json!({
            "name": "Rig kinds",
            "version": "1.0.0",
            "workloads": {
                "demo": {
                    "script": "demo.sh",
                    "workloads": [{ "id": "w", "label": "Declared" }],
                    "replay_command": "printf ok",
                    "safety_class": safety_class
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let script = extension_dir.join("demo.sh");
    std::fs::write(&script, PATH_RUNNER).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let component = home.join("rigsite");
    std::fs::create_dir_all(&component).unwrap();
    std::fs::write(
        component.join("homeboy.json"),
        r#"{"id":"consumer","extensions":{"rigkinds":{}}}"#,
    )
    .unwrap();
    component
}

#[test]
fn rig_workload_listings_carry_the_rig_source_and_file_stem_id() {
    let files = vec![RigWorkloadFile {
        id: "checkout-create-order".to_string(),
        path: PathBuf::from("/tmp/rig/checkout-create-order.json"),
    }];

    let listings = list::rig_workloads(&files, "studio");

    assert_eq!(listings[0].id, "checkout-create-order");
    assert_eq!(listings[0].source, "rig:studio");
    assert_eq!(
        listings[0].label.as_deref(),
        Some("checkout-create-order.json")
    );
    let mut merged = listings;
    merged.extend(list::rig_workloads(&files, "studio"));
    assert_eq!(
        list::duplicate_ids(&merged),
        vec!["checkout-create-order".to_string()]
    );
}

#[test]
fn a_workload_file_can_only_raise_a_kinds_safety_class() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("workload.json");

    std::fs::write(&file, r#"{"safety_class":"read_only"}"#).unwrap();
    let lowered = safety::effective_safety_class(WorkloadSafetyClass::Destructive, Some(&file));
    assert_eq!(lowered.class, WorkloadSafetyClass::Destructive);
    assert_eq!(lowered.source, safety::SAFETY_SOURCE_KIND);
    let confirmed = safety::effective_safety_class(WorkloadSafetyClass::ReadOnly, Some(&file));
    assert_eq!(confirmed.class, WorkloadSafetyClass::ReadOnly);
    assert_eq!(confirmed.source, safety::SAFETY_SOURCE_WORKLOAD_FILE);

    std::fs::write(&file, r#"{"safety_class":"destructive"}"#).unwrap();
    let raised = safety::effective_safety_class(WorkloadSafetyClass::ReadOnly, Some(&file));
    assert_eq!(raised.class, WorkloadSafetyClass::Destructive);
    assert_eq!(raised.source, safety::SAFETY_SOURCE_WORKLOAD_FILE);

    std::fs::write(&file, "not json").unwrap();
    let unparseable = safety::effective_safety_class(WorkloadSafetyClass::ReadOnly, Some(&file));
    assert_eq!(unparseable.class, WorkloadSafetyClass::ReadOnly);
    assert_eq!(unparseable.source, safety::SAFETY_SOURCE_KIND);

    let absent = safety::effective_safety_class(
        WorkloadSafetyClass::ReadOnly,
        Some(&dir.path().join("absent.json")),
    );
    assert_eq!(absent.class, WorkloadSafetyClass::ReadOnly);
    assert_eq!(absent.source, safety::SAFETY_SOURCE_KIND);
}

#[test]
fn run_exports_rig_workload_paths_and_a_file_can_escalate_safety() {
    crate::test_support::with_isolated_home(|home| {
        let component = install_rig_fixture(home.path(), "read_only");
        let resolved = resolve_kind("demo", target(&component)).unwrap();

        let workload_file = home.path().join("workloads/checkout.json");
        std::fs::create_dir_all(workload_file.parent().unwrap()).unwrap();
        std::fs::write(&workload_file, r#"{"safety_class":"destructive"}"#).unwrap();
        let rig_files = vec![RigWorkloadFile {
            id: "checkout".to_string(),
            path: workload_file.clone(),
        }];

        let mut listings = list::declared_workloads(&resolved);
        listings.extend(list::rig_workloads(&rig_files, "studio"));
        assert_eq!(
            listings
                .iter()
                .map(|listing| (listing.id.as_str(), listing.source.as_str()))
                .collect::<Vec<_>>(),
            vec![("w", "manifest"), ("checkout", "rig:studio")]
        );
        assert!(list::duplicate_ids(&listings).is_empty());

        let effective = safety::effective_safety_class(
            resolved.config.safety_class,
            selected_rig_workload(&rig_files, Some("checkout")).map(|file| file.path.as_path()),
        );
        assert_eq!(effective.class, WorkloadSafetyClass::Destructive);
        assert_eq!(effective.source, safety::SAFETY_SOURCE_WORKLOAD_FILE);
        let proof_file = home.path().join("proof.json");
        std::fs::write(&proof_file, proof().to_string()).unwrap();
        let request = |allow: bool, proof_path: Option<&Path>| DestructiveRequest {
            allow_destructive: allow,
            isolation_proof: proof_path.map(Path::to_path_buf),
        };
        let refused =
            safety::authorize("demo", effective.class, &request(false, Some(&proof_file)))
                .unwrap_err();
        assert!(refused.message.contains("--allow-destructive"));
        let authorization =
            safety::authorize("demo", effective.class, &request(true, Some(&proof_file)))
                .unwrap()
                .expect("authorized");

        let workload_path = workload_file.to_string_lossy().to_string();
        let execute =
            |workload_id: &'static str,
             run_id: &'static str,
             authorization: Option<&super::safety::DestructiveAuthorization>| {
                workload_run::run(WorkloadRunRequest {
                    resolved: &resolved,
                    workload_id: Some(workload_id),
                    run_id: Some(run_id),
                    seed: None,
                    rig_id: Some("studio"),
                    rig_workloads: &rig_files,
                    settings: &[],
                    settings_json: &[],
                    script_args: &[],
                    timeout: None,
                    authorization,
                    baseline: BaselineFlags {
                        baseline: false,
                        ignore_baseline: true,
                        ratchet: false,
                    },
                    command: "homeboy workload run --kind demo --rig studio".to_string(),
                })
                .unwrap()
            };
        let escalated = execute("checkout", "rig-run-a", Some(&authorization));
        let payload = &escalated.result.as_ref().unwrap().payload;
        assert_eq!(payload["workload_path"], workload_path);
        assert_eq!(payload["extra_paths"], workload_path);
        let store = ObservationStore::open_initialized().unwrap();
        let run = store.get_run("rig-run-a").unwrap().unwrap();
        assert_eq!(run.metadata_json["safety_class"], "destructive");
        assert_eq!(run.metadata_json["safety_class_source"], "workload_file");
        assert_eq!(run.metadata_json["workload_path"], workload_path);

        let manifest = execute("w", "rig-run-b", None);
        let payload = &manifest.result.as_ref().unwrap().payload;
        assert_eq!(payload["workload_path"], "");
        assert_eq!(payload["extra_paths"], workload_path);
        let run = store.get_run("rig-run-b").unwrap().unwrap();
        assert_eq!(run.metadata_json["safety_class"], "read_only");
        assert_eq!(run.metadata_json["safety_class_source"], "kind");
        assert!(run.metadata_json.get("workload_path").is_none());
    });
}

#[test]
fn replay_reuses_the_recorded_rig_workload_path() {
    crate::test_support::with_isolated_home(|home| {
        let component = install_rig_fixture(home.path(), "read_only");
        let resolved = resolve_kind("demo", target(&component)).unwrap();
        let workload_file = home.path().join("workloads/checkout.json");
        std::fs::create_dir_all(workload_file.parent().unwrap()).unwrap();
        std::fs::write(&workload_file, "{}").unwrap();
        let rig_files = vec![RigWorkloadFile {
            id: "checkout".to_string(),
            path: workload_file.clone(),
        }];
        workload_run::run(WorkloadRunRequest {
            resolved: &resolved,
            workload_id: Some("checkout"),
            run_id: Some("rig-run-src"),
            seed: None,
            rig_id: Some("studio"),
            rig_workloads: &rig_files,
            settings: &[],
            settings_json: &[],
            script_args: &[],
            timeout: None,
            authorization: None,
            baseline: BaselineFlags {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            command: "homeboy workload run --kind demo --rig studio".to_string(),
        })
        .unwrap();

        // The replayed kind declares no replay_command, so only the planned
        // env is observable: it must carry the source run's recorded
        // workload path as HOMEBOY_WORKLOAD_PATH.
        let store = ObservationStore::open_initialized().unwrap();
        let mut source = stored::load(&store, "rig-run-src", Some("demo")).unwrap();
        assert_eq!(
            source.workload_path().map(str::to_string),
            Some(workload_file.to_string_lossy().to_string())
        );
        source.result = WorkloadResult::parse(
            serde_json::to_vec(&json!({
                "schema": WORKLOAD_RESULT_SCHEMA,
                "kind": "demo",
                "workload_id": "checkout",
                "status": "pass",
                "cases": [{ "case_id": "c1", "status": "pass" }],
            }))
            .unwrap()
            .as_slice(),
            "demo",
            None,
        )
        .unwrap();
        let planned = case::execute(CaseRequest {
            mode: CaseMode::Replay,
            resolved: &resolved,
            source: &source,
            case_id: "c1",
            rig_workloads: &[],
            settings: &[],
            settings_json: &[],
            script_args: &[],
            timeout: None,
            authorization: None,
            dry_run: true,
        })
        .unwrap();
        assert!(planned.env.iter().any(|item| {
            item.name == env::PATH && item.value == workload_file.to_string_lossy()
        }));
    });
}
