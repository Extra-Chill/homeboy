use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use homeboy::core::extension::trace::{self as extension_trace, TraceCommandOutput};
use serde::Serialize;

use super::compare_targets::run_compare_targets;
use super::TraceArgs;
use crate::commands::CmdResult;
use homeboy::core::trace_compare::write_json_artifact;

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
    homeboy::core::trace_compare::prepare_compare_bundle_dir(&output_dir)?;

    let mut cells = Vec::new();
    let mut scenario_entries = Vec::new();
    let mut failure_count = 0;
    for (index, scenario) in scenarios.iter().enumerate() {
        let scenario_slug = sanitize_path_component(scenario);
        let scenario_dir = output_dir.join(format!("{:03}-{}", index + 1, scenario_slug));
        homeboy::core::trace_compare::prepare_compare_bundle_dir(&scenario_dir)?;

        let mut compare_args = args.clone();
        compare_args.comp.component = Some("compare".to_string());
        compare_args.scenario = Some(component.clone());
        compare_args.compare_after = Some(PathBuf::from(scenario));
        compare_args.output_dir = Some(scenario_dir.clone());

        let command = compare_command(&compare_args, &component, scenario, &scenario_dir);
        homeboy::core::trace_compare::write_compare_bundle_scenario_log(
            &scenario_dir.join("scenario.log"),
            &command,
            "running",
            None,
        )?;
        let (passed, status, exit_code, failure) = match run_compare_targets(compare_args) {
            Ok((TraceCommandOutput::Compare(compare), exit_code)) => {
                write_json_artifact(&scenario_dir.join("scenario.compare.json"), &compare)?;
                homeboy::core::trace_compare::write_compare_bundle_text(
                    &scenario_dir.join("scenario.compare.md"),
                    &super::output::render_trace_compare_evidence_markdown(&compare),
                )?;
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
                homeboy::core::trace_compare::write_compare_bundle_error_scenario(
                    &scenario_dir,
                    &component,
                    scenario,
                    &failure,
                )?;
                (false, "error".to_string(), exit_code, Some(failure))
            }
            Err(err) => {
                let failure = err.to_string();
                homeboy::core::trace_compare::write_compare_bundle_error_scenario(
                    &scenario_dir,
                    &component,
                    scenario,
                    &failure,
                )?;
                (false, "error".to_string(), 1, Some(failure))
            }
        };
        homeboy::core::trace_compare::write_compare_bundle_scenario_log(
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
    homeboy::core::trace_compare::write_compare_bundle_text(
        &readme_path,
        &render_compare_bundle_readme(&manifest),
    )?;

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
    homeboy::core::trace_compare::write_compare_bundle_text(
        &summary_path,
        &super::output::render_scenario_matrix_markdown(&output),
    )?;

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
