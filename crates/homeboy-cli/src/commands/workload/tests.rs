use std::path::Path;

use clap::Parser;
use serde_json::{json, Value};

use super::{run, WorkloadArgs, WorkloadCommand, WorkloadOutput};
use crate::cli_surface::{Cli, Commands};

fn parse(args: &[&str]) -> Result<WorkloadArgs, clap::Error> {
    let mut argv = vec!["homeboy", "workload"];
    argv.extend_from_slice(args);
    Cli::try_parse_from(argv).map(|cli| match cli.command {
        Commands::Workload(args) => args,
        _ => panic!("not a workload command"),
    })
}

#[test]
fn every_subcommand_parses_with_a_kind() {
    let run = parse(&[
        "run",
        "--kind",
        "fuzz",
        "pkg",
        "--workload",
        "parser",
        "--seed",
        "7",
        "--run-id",
        "r1",
        "--baseline",
        "--",
        "--extra",
    ])
    .unwrap();
    let WorkloadCommand::Run(run) = run.command else {
        panic!("run")
    };
    assert_eq!(run.kind, "fuzz");
    assert_eq!(run.target.comp.component.as_deref(), Some("pkg"));
    assert_eq!(run.workload.as_deref(), Some("parser"));
    assert!(run.baseline_args.baseline);
    assert_eq!(run.args, vec!["--extra".to_string()]);

    assert!(matches!(
        parse(&["list", "--kind", "fuzz", "--rig", "studio"])
            .unwrap()
            .command,
        WorkloadCommand::List(_)
    ));
    assert!(matches!(
        parse(&[
            "compare",
            "--kind",
            "fuzz",
            "--from-run",
            "a",
            "--to-run",
            "b"
        ])
        .unwrap()
        .command,
        WorkloadCommand::Compare(_)
    ));
    assert!(matches!(
        parse(&[
            "replay",
            "--kind",
            "fuzz",
            "--run-id",
            "a",
            "--case-id",
            "c",
            "--dry-run"
        ])
        .unwrap()
        .command,
        WorkloadCommand::Replay(_)
    ));
    assert!(matches!(
        parse(&[
            "minimize",
            "--kind",
            "fuzz",
            "--run-id",
            "a",
            "--case-id",
            "c"
        ])
        .unwrap()
        .command,
        WorkloadCommand::Minimize(_)
    ));
    assert!(matches!(
        parse(&["inspect", "run-1"]).unwrap().command,
        WorkloadCommand::Inspect(_)
    ));
    assert!(parse(&["run", "pkg"]).is_err(), "--kind is required");
    assert!(parse(&["compare", "--kind", "fuzz", "--from-run", "a"]).is_err());
}

#[test]
fn allow_destructive_requires_an_isolation_proof_at_parse_time() {
    assert!(parse(&["run", "--kind", "db", "--allow-destructive"]).is_err());
    assert!(parse(&[
        "run",
        "--kind",
        "db",
        "--allow-destructive",
        "--isolation-proof",
        "proof.json"
    ])
    .is_ok());
    assert!(parse(&[
        "replay",
        "--kind",
        "db",
        "--run-id",
        "a",
        "--case-id",
        "c",
        "--allow-destructive"
    ])
    .is_err());
}

#[test]
fn only_run_is_lab_portable() {
    let run = parse(&["run", "--kind", "fuzz", "--extension", "nodejs"]).unwrap();
    let contract = run.lab_contract().expect("run is portable");
    assert_eq!(contract.hot_label, "workload");
    assert!(contract.routing_policy.requires_extension_parity);
    assert_eq!(
        run.lab_route_required_extension_ids(),
        vec!["nodejs".to_string()]
    );
    for args in [
        vec!["list", "--kind", "fuzz"],
        vec!["inspect", "r"],
        vec![
            "compare",
            "--kind",
            "fuzz",
            "--from-run",
            "a",
            "--to-run",
            "b",
        ],
    ] {
        assert!(parse(&args).unwrap().lab_contract().is_none(), "{args:?}");
    }
}

const RUNNER: &str = r#"#!/bin/sh
set -e
p95=$(cat "$HOMEBOY_COMPONENT_PATH/p95")
cat > "$HOMEBOY_WORKLOAD_RESULTS_FILE" <<EOF
{"schema":"homeboy/workload-result/v1","kind":"$HOMEBOY_WORKLOAD_KIND","status":"pass",
 "findings":[{"tool":"db","message":"slow query","fingerprint":"slow:q1"}],
 "cases":[{"case_id":"q1","seed":"$HOMEBOY_WORKLOAD_SEED","status":"pass"}],
 "metrics":{"p95_ms":$p95},
 "payload":{"destructive":"$HOMEBOY_WORKLOAD_ALLOW_DESTRUCTIVE","proof":"$HOMEBOY_WORKLOAD_ISOLATION_PROOF_FILE"}}
EOF
"#;

fn install(home: &Path) -> std::path::PathBuf {
    let extension_dir = homeboy::core::paths::extensions().unwrap().join("dbkinds");
    std::fs::create_dir_all(&extension_dir).unwrap();
    std::fs::write(
        extension_dir.join("dbkinds.json"),
        json!({
            "name": "DB kinds",
            "version": "1.0.0",
            "workloads": {
                "db": {
                    "script": "db.sh",
                    "safety_class": "destructive",
                    "replay_command": "printf '%s' \"$HOMEBOY_WORKLOAD_ALLOW_DESTRUCTIVE\"",
                    "compare": { "metrics": [{ "name": "p95_ms", "direction": "lower_is_better" }] }
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let script = extension_dir.join("db.sh");
    std::fs::write(&script, RUNNER).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let component = home.join("site");
    std::fs::create_dir_all(&component).unwrap();
    std::fs::write(
        component.join("homeboy.json"),
        r#"{"id":"site","extensions":{"dbkinds":{}}}"#,
    )
    .unwrap();
    std::fs::write(component.join("p95"), "100").unwrap();
    let proof = home.join("proof.json");
    std::fs::write(
        &proof,
        json!({
            "schema": "homeboy/isolation-proof/v1",
            "version": 1,
            "runtime_kind": "container",
            "disposable": true,
            "teardown_required": true,
            "mutation_boundary": "throwaway database",
            "proof_artifacts": [{ "kind": "container", "ref": "c1" }],
            "verified_by": "test"
        })
        .to_string(),
    )
    .unwrap();
    component
}

fn to_json(output: WorkloadOutput) -> Value {
    serde_json::to_value(output).unwrap()
}

#[test]
fn destructive_kind_is_refused_without_authorization_then_runs_compares_and_replays() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let component = install(home.path());
        let path = component.to_string_lossy().to_string();
        let proof = home.path().join("proof.json").to_string_lossy().to_string();
        let store = || homeboy::core::observation::ObservationStore::open_initialized().unwrap();

        let refused = run(parse(&[
            "run", "--kind", "db", "site", "--path", &path, "--run-id", "db-0",
        ])
        .unwrap())
        .err()
        .expect("destructive kind refused");
        assert!(refused.message.contains("--allow-destructive"));
        assert!(
            store().get_run("db-0").unwrap().is_none(),
            "refused before a run row exists"
        );

        let authorized = |run_id: &str| {
            run(parse(&[
                "run",
                "--kind",
                "db",
                "site",
                "--path",
                &path,
                "--run-id",
                run_id,
                "--seed",
                "9",
                "--allow-destructive",
                "--isolation-proof",
                &proof,
                "--ignore-baseline",
            ])
            .unwrap())
            .unwrap()
        };
        let (first, exit) = authorized("db-1");
        assert_eq!(exit, 0);
        let first = to_json(first);
        assert_eq!(first["variant"], "run");
        assert_eq!(first["status"], "pass");
        assert_eq!(first["safety_class"], "destructive");
        assert_eq!(first["result"]["payload"]["destructive"], "1");
        assert_eq!(first["result"]["payload"]["proof"], proof);
        assert_eq!(first["result"]["cases"][0]["seed"], "9");
        let artifacts = store().list_artifacts("db-1").unwrap();
        assert!(artifacts
            .iter()
            .any(|artifact| artifact.kind == "isolation_proof"));

        std::fs::write(component.join("p95"), "150").unwrap();
        authorized("db-2");
        let (compared, exit) = run(parse(&[
            "compare",
            "--kind",
            "db",
            "--from-run",
            "db-1",
            "--to-run",
            "db-2",
        ])
        .unwrap())
        .unwrap();
        assert_eq!(exit, 1, "a declared metric regression fails compare");
        let compared = to_json(compared);
        assert_eq!(compared["comparison"]["status"], "worse");
        assert_eq!(
            compared["comparison"]["regressions"][0],
            "metric `p95_ms` regressed"
        );
        assert_eq!(compared["comparison"]["delta"]["findings"]["persisting"], 1);
        assert_eq!(compared["failure"]["code"], "workload.compare_regressed");
        assert!(
            first.get("failure").is_none(),
            "a passing run names no failure"
        );

        let (_, exit) = run(parse(&[
            "compare",
            "--kind",
            "db",
            "--from-run",
            "db-2",
            "--to-run",
            "db-1",
        ])
        .unwrap())
        .unwrap();
        assert_eq!(exit, 0);
        assert!(run(parse(&[
            "compare",
            "--kind",
            "other",
            "--from-run",
            "db-1",
            "--to-run",
            "db-2",
        ])
        .unwrap())
        .is_err());

        let refused_replay = run(parse(&[
            "replay",
            "--kind",
            "db",
            "--run-id",
            "db-1",
            "--case-id",
            "q1",
        ])
        .unwrap())
        .err()
        .expect("destructive replay refused");
        assert!(refused_replay.message.contains("--allow-destructive"));
        let (replayed, exit) = run(parse(&[
            "replay",
            "--kind",
            "db",
            "--run-id",
            "db-1",
            "--case-id",
            "q1",
            "--allow-destructive",
            "--isolation-proof",
            &proof,
        ])
        .unwrap())
        .unwrap();
        assert_eq!(exit, 0);
        let replayed = to_json(replayed);
        assert_eq!(replayed["variant"], "replay");
        assert_eq!(replayed["status"], "passed");
        assert_eq!(replayed["stdout_tail"], "1");
        assert!(replayed["env"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "HOMEBOY_WORKLOAD_SEED" && item["value"] == "9"));

        let (inspected, _) = run(parse(&["inspect", "db-1", "--kind", "db"]).unwrap()).unwrap();
        let inspected = to_json(inspected);
        assert_eq!(inspected["variant"], "inspect");
        assert_eq!(inspected["kind"], "db");
        assert_eq!(inspected["finding_count"], 1);
        assert_eq!(inspected["findings"][0]["fingerprint"], "slow:q1");
        assert_eq!(inspected["metrics"]["p95_ms"], 100.0);
        assert!(inspected.get("result").is_none());

        let (listed, _) =
            run(parse(&["list", "--kind", "db", "site", "--path", &path]).unwrap()).unwrap();
        let listed = to_json(listed);
        assert_eq!(listed["capabilities"]["safety_class"], "destructive");
        assert_eq!(listed["capabilities"]["replay"], true);
        assert_eq!(listed["capabilities"]["minimize"], false);
    });
}
