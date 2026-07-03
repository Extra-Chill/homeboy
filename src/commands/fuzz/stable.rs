use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;

use homeboy::core::error::{Error, Result};

use super::types::{FuzzStableCommand, FuzzStablePlanArgs};
use super::types_extra::{
    FuzzStableCompareCommandOutput, FuzzStablePlanOutput, FuzzStableRunCommandOutput,
};

pub(super) fn run_stable(command: FuzzStableCommand) -> Result<FuzzStablePlanOutput> {
    match command {
        FuzzStableCommand::Plan(args) => run_stable_plan(args),
    }
}

fn run_stable_plan(args: FuzzStablePlanArgs) -> Result<FuzzStablePlanOutput> {
    if args.limit == 0 {
        return Err(Error::validation_invalid_argument(
            "limit",
            "--limit must be a positive integer",
            Some(args.limit.to_string()),
            None,
        ));
    }
    if args.hotspot_limit == 0 {
        return Err(Error::validation_invalid_argument(
            "hotspot-limit",
            "--hotspot-limit must be a positive integer",
            Some(args.hotspot_limit.to_string()),
            None,
        ));
    }

    let manifest = read_manifest(&args.manifest)?;
    let rig_id = resolve_rig_id(&args.manifest, &manifest.rig)?;
    let contracts = selected_contracts(&manifest.contracts, &args.stable_ids)?;
    let run_id_prefix = args
        .run_id_prefix
        .clone()
        .unwrap_or_else(|| format!("stable-{}", Utc::now().format("%Y%m%d")));

    let mut run_commands = Vec::new();
    for contract in contracts {
        if contract.entry_workloads.is_empty() {
            return Err(Error::validation_invalid_argument(
                "manifest",
                "stable workload contracts must declare at least one entry_workloads item",
                Some(contract.id.clone()),
                None,
            ));
        }
        for (index, workload_id) in contract.entry_workloads.iter().enumerate() {
            let run_id = format!(
                "{}-{}-{:02}-{}",
                run_id_prefix,
                contract.id,
                index + 1,
                workload_id
            );
            let mut command = vec![
                "homeboy".to_string(),
                "fuzz".to_string(),
                "run".to_string(),
                "--lab-only".to_string(),
                "--rig".to_string(),
                rig_id.clone(),
                "--workload".to_string(),
                workload_id.clone(),
                "--gate-profile".to_string(),
                "measurement".to_string(),
                "--run-id".to_string(),
                run_id.clone(),
                "--tracker-ref".to_string(),
                format!("stable-workload:{}", contract.id),
            ];
            append_common_run_options(&mut command, &args);
            if let Some(seconds) = contract
                .budgets
                .get("max_duration_seconds")
                .and_then(Value::as_u64)
            {
                command.push("--max-duration".to_string());
                command.push(format!("{seconds}s"));
            }
            for tracker_ref in &args.tracker_refs {
                command.push("--tracker-ref".to_string());
                command.push(tracker_ref.clone());
            }
            run_commands.push(FuzzStableRunCommandOutput {
                stable_workload_id: contract.id.clone(),
                workload_id: workload_id.clone(),
                run_id,
                command,
            });
        }
    }

    Ok(FuzzStablePlanOutput {
        schema: "homeboy/fuzz-stable-lab-command-plan/v1".to_string(),
        command: "fuzz.stable.plan".to_string(),
        manifest: args.manifest.to_string_lossy().to_string(),
        profile_id: manifest.profile_id,
        rig_id: rig_id.clone(),
        local_execution: false,
        run_id_prefix,
        run_commands,
        compare_commands: compare_commands(&args, &rig_id),
        next_steps: vec![
            "Run the planned fuzz commands in Lab, then use the compare commands after two completed runs exist.".to_string(),
            "Keep product-specific workload selection in the manifest; Homeboy only plans declared workload ids.".to_string(),
        ],
    })
}

fn append_common_run_options(command: &mut Vec<String>, args: &FuzzStablePlanArgs) {
    if let Some(runner) = &args.runner {
        command.push("--runner".to_string());
        command.push(runner.clone());
    }
    if let Some(artifact_root) = &args.artifact_root {
        command.push("--artifact-root".to_string());
        command.push(artifact_root.to_string_lossy().to_string());
    }
    if args.detach_after_handoff {
        command.push("--detach-after-handoff".to_string());
    }
}

fn compare_commands(
    args: &FuzzStablePlanArgs,
    rig_id: &str,
) -> Vec<FuzzStableCompareCommandOutput> {
    vec![
        FuzzStableCompareCommandOutput {
            purpose: "list_recent_refs".to_string(),
            command: with_lab_options(
                vec![
                    "homeboy",
                    "runs",
                    "refs",
                    "--kind",
                    "fuzz",
                    "--rig",
                    rig_id,
                    "--status",
                    "completed",
                    "--since",
                    &args.since,
                    "--limit",
                    &args.limit.to_string(),
                    "--aggregate-artifact-kind",
                    "fuzz.report",
                ],
                args,
            ),
        },
        FuzzStableCompareCommandOutput {
            purpose: "trend_elapsed_time".to_string(),
            command: with_lab_options(
                vec![
                    "homeboy",
                    "runs",
                    "compare",
                    "--kind",
                    "fuzz",
                    "--rig",
                    rig_id,
                    "--metric",
                    "total_elapsed_ms",
                    "--limit",
                    &args.limit.to_string(),
                ],
                args,
            ),
        },
        FuzzStableCompareCommandOutput {
            purpose: "compare_hotspots_after_two_runs_complete".to_string(),
            command: with_lab_options(
                vec![
                    "homeboy",
                    "runs",
                    "hotspots",
                    "--baseline-run",
                    "BASELINE_RUN_ID",
                    "--candidate-run",
                    "CANDIDATE_RUN_ID",
                    "--limit",
                    &args.hotspot_limit.to_string(),
                ],
                args,
            ),
        },
    ]
}

fn with_lab_options(parts: Vec<&str>, args: &FuzzStablePlanArgs) -> Vec<String> {
    let mut command: Vec<String> = parts.into_iter().map(str::to_string).collect();
    if let Some(component) = &args.component {
        command.push("--component".to_string());
        command.push(component.clone());
    }
    command.push("--lab-only".to_string());
    if let Some(runner) = &args.runner {
        command.push("--runner".to_string());
        command.push(runner.clone());
    }
    if let Some(artifact_root) = &args.artifact_root {
        command.push("--artifact-root".to_string());
        command.push(artifact_root.to_string_lossy().to_string());
    }
    command
}

fn selected_contracts<'a>(
    contracts: &'a [StableWorkloadContract],
    stable_ids: &[String],
) -> Result<Vec<&'a StableWorkloadContract>> {
    if stable_ids.is_empty() {
        return Ok(contracts.iter().collect());
    }
    let selected: std::collections::BTreeSet<_> = stable_ids.iter().map(String::as_str).collect();
    let filtered: Vec<_> = contracts
        .iter()
        .filter(|contract| selected.contains(contract.id.as_str()))
        .collect();
    let found: std::collections::BTreeSet<_> = filtered
        .iter()
        .map(|contract| contract.id.as_str())
        .collect();
    let missing: Vec<_> = selected.difference(&found).copied().collect();
    if !missing.is_empty() {
        return Err(Error::validation_invalid_argument(
            "stable-id",
            "unknown stable workload id(s)",
            Some(missing.join(", ")),
            None,
        ));
    }
    Ok(filtered)
}

fn read_manifest(path: &Path) -> Result<StableWorkloadManifest> {
    let content = fs::read_to_string(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    serde_json::from_str(&content).map_err(|error| {
        Error::validation_invalid_argument(
            "manifest",
            "stable workload manifest must be valid JSON",
            Some(error.to_string()),
            None,
        )
    })
}

fn resolve_rig_id(manifest_path: &Path, rig_ref: &str) -> Result<String> {
    if rig_ref.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "manifest.rig",
            "stable workload manifest must declare rig",
            None,
            None,
        ));
    }
    let package_root = manifest_path
        .parent()
        .and_then(|parent| {
            (parent.file_name().and_then(|name| name.to_str()) == Some("manifests"))
                .then(|| parent.parent())
                .flatten()
        })
        .or_else(|| manifest_path.parent())
        .unwrap_or_else(|| Path::new("."));
    let rig_path = if Path::new(rig_ref).is_absolute() {
        PathBuf::from(rig_ref)
    } else {
        package_root.join(rig_ref)
    };
    let content = fs::read_to_string(&rig_path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read manifest rig ref {}", rig_path.display())),
        )
    })?;
    let value: Value = serde_json::from_str(&content).map_err(|error| {
        Error::validation_invalid_argument(
            "manifest.rig",
            "manifest rig ref must point at valid rig JSON",
            Some(error.to_string()),
            None,
        )
    })?;
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "manifest.rig",
                "manifest rig ref must point at a rig with a non-empty id",
                Some(rig_path.to_string_lossy().to_string()),
                None,
            )
        })
}

#[derive(Deserialize)]
struct StableWorkloadManifest {
    #[allow(dead_code)]
    schema: Option<String>,
    profile_id: Option<String>,
    rig: String,
    #[serde(default)]
    contracts: Vec<StableWorkloadContract>,
}

#[derive(Deserialize)]
struct StableWorkloadContract {
    id: String,
    #[serde(default)]
    entry_workloads: Vec<String>,
    #[serde(default)]
    budgets: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_plan_resolves_rig_and_builds_lab_commands() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("demo");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(rig_dir.join("rig.json"), r#"{"id":"demo-rig"}"#).expect("rig");
        let manifests = temp.path().join("manifests");
        fs::create_dir_all(&manifests).expect("manifests");
        let manifest = manifests.join("stable-workloads.json");
        fs::write(
            &manifest,
            r#"{
                "schema":"example/stable-workloads/v1",
                "profile_id":"stable-demo",
                "rig":"rigs/demo/rig.json",
                "contracts":[{"id":"api","entry_workloads":["read"],"budgets":{"max_duration_seconds":60}}]
            }"#,
        )
        .expect("manifest");

        let output = run_stable_plan(FuzzStablePlanArgs {
            manifest,
            stable_ids: vec!["api".to_string()],
            runner: Some("lab".to_string()),
            artifact_root: None,
            run_id_prefix: Some("stable-demo".to_string()),
            tracker_refs: vec!["issue:1".to_string()],
            detach_after_handoff: true,
            component: Some("component-a".to_string()),
            since: "7d".to_string(),
            limit: 5,
            hotspot_limit: 3,
        })
        .expect("plan");

        assert_eq!(output.rig_id, "demo-rig");
        assert_eq!(output.run_commands.len(), 1);
        assert_eq!(
            output.run_commands[0].command,
            vec![
                "homeboy",
                "fuzz",
                "run",
                "--lab-only",
                "--rig",
                "demo-rig",
                "--workload",
                "read",
                "--gate-profile",
                "measurement",
                "--run-id",
                "stable-demo-api-01-read",
                "--tracker-ref",
                "stable-workload:api",
                "--runner",
                "lab",
                "--detach-after-handoff",
                "--max-duration",
                "60s",
                "--tracker-ref",
                "issue:1",
            ]
        );
        assert!(output.compare_commands[0]
            .command
            .windows(2)
            .any(|pair| pair == ["--component", "component-a"]));
        assert_eq!(
            output.compare_commands[2].command,
            vec![
                "homeboy",
                "runs",
                "hotspots",
                "--baseline-run",
                "BASELINE_RUN_ID",
                "--candidate-run",
                "CANDIDATE_RUN_ID",
                "--limit",
                "3",
                "--component",
                "component-a",
                "--lab-only",
                "--runner",
                "lab",
            ]
        );
    }

    #[test]
    fn stable_plan_rejects_unknown_stable_id() {
        let contracts = vec![StableWorkloadContract {
            id: "known".to_string(),
            entry_workloads: vec!["workload".to_string()],
            budgets: serde_json::Map::new(),
        }];
        let result = selected_contracts(&contracts, &["missing".to_string()]);

        let err = match result {
            Ok(_) => panic!("unknown stable id should fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unknown stable workload id"));
    }
}
