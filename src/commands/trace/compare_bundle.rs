use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use homeboy::core::extension::trace::{self as extension_trace, TraceCommandOutput};
use serde::Serialize;

use super::compare_targets::run_compare_targets;
use super::matrix::write_json_artifact;
use super::TraceArgs;
use crate::commands::CmdResult;

pub(super) fn run_compare_bundle(args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    if args.baseline_target.is_none() || args.candidate.is_none() {
        return Err(homeboy::core::Error::validation_missing_argument(vec![
            "--baseline-target".to_string(),
            "--candidate".to_string(),
        ]));
    }

    let component = args.scenario.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec!["component".to_string()])
    })?;
    let scenarios = compare_bundle_scenarios(&args)?;
    let output_dir = args.output_dir.clone().unwrap_or_else(|| {
        PathBuf::from(".homeboy")
            .join("trace-compare-bundles")
            .join(format!(
                "{}-{}",
                sanitize_path_component(&component),
                chrono::Utc::now().format("%Y%m%d%H%M%S")
            ))
    });
    std::fs::create_dir_all(&output_dir).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to create trace compare bundle output dir {}: {}",
                output_dir.display(),
                err
            ),
            Some("trace.compare_bundle.output_dir".to_string()),
        )
    })?;

    let mut cells = Vec::new();
    let mut scenario_entries = Vec::new();
    let mut failure_count = 0;
    for (index, scenario) in scenarios.iter().enumerate() {
        let scenario_slug = sanitize_path_component(scenario);
        let scenario_dir = output_dir.join(format!("{:03}-{}", index + 1, scenario_slug));
        std::fs::create_dir_all(&scenario_dir).map_err(|err| {
            homeboy::core::Error::internal_io(
                format!(
                    "Failed to create trace compare bundle scenario dir {}: {}",
                    scenario_dir.display(),
                    err
                ),
                Some("trace.compare_bundle.scenario_dir".to_string()),
            )
        })?;

        let mut compare_args = args.clone();
        compare_args.comp.component = Some("compare".to_string());
        compare_args.scenario = Some(component.clone());
        compare_args.compare_after = Some(PathBuf::from(scenario));
        compare_args.output_dir = Some(scenario_dir.clone());

        let command = compare_command(&compare_args, &component, scenario, &scenario_dir);
        write_scenario_log(
            &scenario_dir.join("scenario.log"),
            &command,
            "running",
            None,
        )?;
        let (passed, status, exit_code, failure) = match run_compare_targets(compare_args) {
            Ok((TraceCommandOutput::Compare(compare), exit_code)) => {
                write_json_artifact(&scenario_dir.join("scenario.compare.json"), &compare)?;
                std::fs::write(
                    scenario_dir.join("scenario.compare.md"),
                    super::output::render_trace_compare_evidence_markdown(&compare),
                )
                .map_err(|err| {
                    homeboy::core::Error::internal_io(
                        format!(
                            "Failed to write trace compare bundle scenario markdown {}: {}",
                            scenario_dir.join("scenario.compare.md").display(),
                            err
                        ),
                        Some("trace.compare_bundle.scenario_markdown".to_string()),
                    )
                })?;
                let failed = exit_code != 0;
                (
                    !failed,
                    if failed { "fail" } else { "pass" }.to_string(),
                    exit_code,
                    None,
                )
            }
            Ok((_, exit_code)) => {
                let failure = "trace compare-bundle expected compare output".to_string();
                write_error_scenario_artifacts(&scenario_dir, &component, scenario, &failure)?;
                (false, "error".to_string(), exit_code, Some(failure))
            }
            Err(err) => {
                let failure = err.to_string();
                write_error_scenario_artifacts(&scenario_dir, &component, scenario, &failure)?;
                (false, "error".to_string(), 1, Some(failure))
            }
        };
        write_scenario_log(
            &scenario_dir.join("scenario.log"),
            &command,
            &status,
            failure.as_deref(),
        )?;
        if !passed {
            failure_count += 1;
        }

        let mut axes = BTreeMap::new();
        axes.insert("scenario".to_string(), scenario.clone());
        cells.push(extension_trace::TraceScenarioMatrixCellOutput {
            index,
            label: scenario.clone(),
            axes,
            passed,
            status: status.clone(),
            exit_code,
            artifact_path: scenario_dir
                .join("compare.json")
                .to_string_lossy()
                .to_string(),
            artifact_dir: scenario_dir.to_string_lossy().to_string(),
            output_path: scenario_dir
                .join("scenario.compare.json")
                .to_string_lossy()
                .to_string(),
            failure: failure.clone(),
        });
        scenario_entries.push(TraceCompareBundleScenarioManifest {
            scenario: scenario.clone(),
            status,
            exit_code,
            directory: relative_path(&output_dir, &scenario_dir),
            compare_json: relative_path(&output_dir, &scenario_dir.join("compare.json")),
            compare_markdown: relative_path(&output_dir, &scenario_dir.join("summary.md")),
            evidence_json: relative_path(&output_dir, &scenario_dir.join("scenario.compare.json")),
            evidence_markdown: relative_path(
                &output_dir,
                &scenario_dir.join("scenario.compare.md"),
            ),
            log: relative_path(&output_dir, &scenario_dir.join("scenario.log")),
            command,
            failure,
        });
    }

    let manifest_path = output_dir.join("manifest.json");
    let readme_path = output_dir.join("README.md");
    let summary_path = output_dir.join("summary.md");
    let bundle_json_path = output_dir.join("bundle.json");
    let exit_code = if failure_count == 0 { 0 } else { 1 };
    let manifest = TraceCompareBundleManifest {
        command: "trace.compare-bundle",
        timestamp: chrono::Utc::now().to_rfc3339(),
        component: component.clone(),
        baseline_target: args.baseline_target.clone().unwrap_or_default(),
        candidate_target: args.candidate.clone().unwrap_or_default(),
        profile: args.profile.clone(),
        repeat: args.repeat.max(1),
        schedule: args.schedule.as_str().to_string(),
        output_dir: output_dir.to_string_lossy().to_string(),
        status: if failure_count == 0 { "pass" } else { "fail" }.to_string(),
        exit_code,
        scenarios: scenario_entries,
    };
    write_json_artifact(&manifest_path, &manifest)?;
    std::fs::write(&readme_path, render_compare_bundle_readme(&manifest)).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to write trace compare bundle README {}: {}",
                readme_path.display(),
                err
            ),
            Some("trace.compare_bundle.readme".to_string()),
        )
    })?;

    let output = extension_trace::TraceScenarioMatrixOutput {
        command: "trace.compare-bundle",
        passed: failure_count == 0,
        status: manifest.status.clone(),
        component,
        scenario_id: "compare-bundle".to_string(),
        output_dir: output_dir.to_string_lossy().to_string(),
        matrix_path: manifest_path.to_string_lossy().to_string(),
        summary_path: summary_path.to_string_lossy().to_string(),
        axes: vec![extension_trace::TraceScenarioMatrixAxisOutput {
            name: "scenario".to_string(),
            values: scenarios,
        }],
        cell_count: cells.len(),
        failure_count,
        exit_code,
        cells,
    };
    write_json_artifact(&bundle_json_path, &output)?;
    std::fs::write(
        &summary_path,
        super::output::render_scenario_matrix_markdown(&output),
    )
    .map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to write trace compare bundle summary {}: {}",
                summary_path.display(),
                err
            ),
            Some("trace.compare_bundle.summary".to_string()),
        )
    })?;

    Ok((TraceCommandOutput::ScenarioMatrix(output), exit_code))
}

#[derive(Serialize)]
struct TraceCompareBundleManifest {
    command: &'static str,
    timestamp: String,
    component: String,
    baseline_target: String,
    candidate_target: String,
    profile: Option<String>,
    repeat: usize,
    schedule: String,
    output_dir: String,
    status: String,
    exit_code: i32,
    scenarios: Vec<TraceCompareBundleScenarioManifest>,
}

#[derive(Serialize)]
struct TraceCompareBundleScenarioManifest {
    scenario: String,
    status: String,
    exit_code: i32,
    directory: String,
    compare_json: String,
    compare_markdown: String,
    evidence_json: String,
    evidence_markdown: String,
    log: String,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

fn compare_bundle_scenarios(args: &TraceArgs) -> homeboy::core::Result<Vec<String>> {
    let raw = args
        .scenario_arg
        .clone()
        .or_else(|| {
            args.compare_after
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())
        })
        .ok_or_else(|| {
            homeboy::core::Error::validation_missing_argument(vec!["scenario list".to_string()])
        })?;
    let scenarios = raw
        .split(',')
        .map(str::trim)
        .filter(|scenario| !scenario.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if scenarios.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "scenario list",
            "trace compare-bundle requires at least one scenario",
            None,
            None,
        ));
    }
    Ok(scenarios)
}

fn compare_command(
    args: &TraceArgs,
    component: &str,
    scenario: &str,
    scenario_dir: &Path,
) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "trace".to_string(),
        "compare".to_string(),
        component.to_string(),
        scenario.to_string(),
        "--baseline-target".to_string(),
        args.baseline_target.clone().unwrap_or_default(),
        "--candidate".to_string(),
        args.candidate.clone().unwrap_or_default(),
        "--repeat".to_string(),
        args.repeat.max(1).to_string(),
        "--schedule".to_string(),
        args.schedule.as_str().to_string(),
        "--output-dir".to_string(),
        scenario_dir.to_string_lossy().to_string(),
    ];
    if let Some(rig) = args.rig.as_deref() {
        parts.extend(["--rig".to_string(), rig.to_string()]);
    }
    if let Some(profile) = args.profile.as_deref() {
        parts.extend(["--profile".to_string(), profile.to_string()]);
    }
    if args.canonical {
        parts.push("--canonical".to_string());
    }
    parts.join(" ")
}

fn write_scenario_log(
    path: &Path,
    command: &str,
    status: &str,
    failure: Option<&str>,
) -> homeboy::core::Result<()> {
    let mut log = format!("command: {}\nstatus: {}\n", command, status);
    if let Some(failure) = failure {
        log.push_str(&format!("failure: {}\n", failure));
    }
    std::fs::write(path, log).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to write trace compare bundle log {}: {}",
                path.display(),
                err
            ),
            Some("trace.compare_bundle.log".to_string()),
        )
    })
}

fn write_error_scenario_artifacts(
    scenario_dir: &Path,
    component: &str,
    scenario: &str,
    failure: &str,
) -> homeboy::core::Result<()> {
    let value = serde_json::json!({
        "command": "trace.compare-bundle.scenario",
        "passed": false,
        "status": "error",
        "exit_code": 1,
        "component": component,
        "scenario_id": scenario,
        "failure": failure,
    });
    write_json_artifact(&scenario_dir.join("scenario.compare.json"), &value)?;
    std::fs::write(
        scenario_dir.join("scenario.compare.md"),
        format!(
            "# Trace Compare Scenario Error\n\n- **Component:** `{}`\n- **Scenario:** `{}`\n- **Status:** `error`\n- **Failure:** {}\n",
            component, scenario, failure
        ),
    )
    .map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to write trace compare bundle scenario error markdown {}: {}",
                scenario_dir.join("scenario.compare.md").display(),
                err
            ),
            Some("trace.compare_bundle.scenario_error_markdown".to_string()),
        )
    })
}

fn render_compare_bundle_readme(manifest: &TraceCompareBundleManifest) -> String {
    let mut out = format!(
        "# Trace Compare Bundle\n\n- **Component:** `{}`\n- **Baseline:** `{}`\n- **Candidate:** `{}`\n- **Status:** `{}`\n- **Repeat:** `{}`\n- **Schedule:** `{}`\n\n## Scenarios\n\n| Scenario | Status | JSON | Markdown | Log |\n|---|---|---|---|---|\n",
        manifest.component,
        manifest.baseline_target,
        manifest.candidate_target,
        manifest.status,
        manifest.repeat,
        manifest.schedule
    );
    for scenario in &manifest.scenarios {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | `{}` | `{}` |\n",
            scenario.scenario,
            scenario.status,
            scenario.evidence_json,
            scenario.evidence_markdown,
            scenario.log
        ));
    }
    out
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if sanitized.is_empty() {
        "scenario".to_string()
    } else {
        sanitized
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}
