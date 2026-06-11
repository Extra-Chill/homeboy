//! Bench artifact contract validation.

use crate::core::error::{Error, Result};

use super::artifact::BenchArtifact;
use super::parsing::{BenchResults, BenchScenario};

pub(crate) fn validate_artifact_paths(results: &BenchResults, rig_id: Option<&str>) -> Result<()> {
    for scenario in &results.scenarios {
        validate_artifacts_for_context(
            ArtifactPathScope::new(&results.component_id, scenario, rig_id, "scenario", None),
            &scenario.artifacts,
        )?;

        if let Some(runs) = &scenario.runs {
            for (run_index, run) in runs.iter().enumerate() {
                validate_artifacts_for_context(
                    ArtifactPathScope::new(
                        &results.component_id,
                        scenario,
                        rig_id,
                        "iteration",
                        Some(run_index + 1),
                    ),
                    &run.artifacts,
                )?;
            }
        }
    }

    Ok(())
}

fn validate_artifacts_for_context(
    scope: ArtifactPathScope<'_>,
    artifacts: &std::collections::BTreeMap<String, BenchArtifact>,
) -> Result<()> {
    for (artifact_key, artifact) in artifacts {
        if artifact
            .path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(empty_artifact_path_error(
                scope.context,
                scope.component_id,
                &scope.scenario.id,
                scope.scenario.file.as_deref(),
                artifact_key,
            ));
        }
    }

    Ok(())
}

struct ArtifactPathScope<'a> {
    component_id: &'a str,
    scenario: &'a BenchScenario,
    context: ArtifactPathContext<'a>,
}

impl<'a> ArtifactPathScope<'a> {
    fn new(
        component_id: &'a str,
        scenario: &'a BenchScenario,
        rig_id: Option<&'a str>,
        phase: &'a str,
        iteration: Option<usize>,
    ) -> Self {
        Self {
            component_id,
            scenario,
            context: ArtifactPathContext {
                rig_id,
                phase,
                iteration,
            },
        }
    }
}

#[derive(Clone, Copy)]
struct ArtifactPathContext<'a> {
    rig_id: Option<&'a str>,
    phase: &'a str,
    iteration: Option<usize>,
}

fn empty_artifact_path_error(
    context: ArtifactPathContext<'_>,
    component_id: &str,
    scenario_id: &str,
    workload_id: Option<&str>,
    artifact_key: &str,
) -> Error {
    Error::validation_invalid_argument(
        "artifacts.path",
        format!(
            "bench artifact path is empty for {}; artifact paths must be non-empty. Omit optional artifacts, or provide a real diagnostics file/directory when evidence is available.",
            artifact_context_parts(context, component_id, scenario_id, workload_id, artifact_key).join(", ")
        ),
        Some(artifact_key.to_string()),
        None,
    )
}

fn artifact_context_parts(
    context: ArtifactPathContext<'_>,
    component_id: &str,
    scenario_id: &str,
    workload_id: Option<&str>,
    artifact_key: &str,
) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(rig_id) = context.rig_id {
        parts.push(format!("rig id `{}`", rig_id));
    }
    parts.push(format!("component id `{}`", component_id));
    if let Some(workload_id) = workload_id {
        parts.push(format!("workload id `{}`", workload_id));
    }
    parts.push(format!("scenario id `{}`", scenario_id));
    parts.push(format!("phase `{}`", context.phase));
    if let Some(iteration) = context.iteration {
        parts.push(format!("iteration {}", iteration));
    }
    parts.push(format!("artifact key `{}`", artifact_key));
    parts
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::core::extension::bench::parsing::{BenchMetrics, BenchRunSnapshot};

    #[test]
    fn test_validate_artifact_paths() {
        let mut results = BenchResults {
            component_id: "studio".to_string(),
            iterations: 1,
            provenance: Default::default(),
            run_metadata: None,
            metadata: BTreeMap::new(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: BTreeMap::new(),
            diagnostics: Vec::new(),
            phase_events: Vec::new(),
            phase_summaries: Vec::new(),
            failure_classification: None,
            responsiveness: None,
            budget_findings: Vec::new(),
            scenarios: vec![BenchScenario {
                id: "site_build".to_string(),
                file: Some("bench/site-build.bench.mjs".to_string()),
                source: None,
                default_iterations: None,
                tags: Vec::new(),
                iterations: 1,
                metrics: BenchMetrics::default(),
                metric_groups: BTreeMap::new(),
                timeline: Vec::new(),
                span_definitions: Vec::new(),
                span_results: Vec::new(),
                gates: Vec::new(),
                gate_results: Vec::new(),
                metadata: BTreeMap::new(),
                provenance: Default::default(),
                passed: true,
                memory: None,
                artifacts: BTreeMap::new(),
                diagnostics: Vec::new(),
                runs: Some(vec![BenchRunSnapshot {
                    metrics: BenchMetrics::default(),
                    metric_groups: BTreeMap::new(),
                    timeline: Vec::new(),
                    span_definitions: Vec::new(),
                    span_results: Vec::new(),
                    memory: None,
                    artifacts: BTreeMap::from([(
                        "visual_comparison_dir".to_string(),
                        BenchArtifact {
                            path: Some("".to_string()),
                            url: None,
                            artifact_type: None,
                            kind: None,
                            label: None,
                            observation_artifact_id: None,
                            ..BenchArtifact::default()
                        },
                    )]),
                    diagnostics: Vec::new(),
                }]),
                runs_summary: None,
            }],
            metric_policies: BTreeMap::new(),
            metric_policy_presets: BTreeMap::new(),
        };

        let err = validate_artifact_paths(&results, Some("studio-bfb"))
            .expect_err("empty run artifact path should fail");
        let message = err.to_string();
        assert!(message.contains("rig id `studio-bfb`"));
        assert!(message.contains("component id `studio`"));
        assert!(message.contains("workload id `bench/site-build.bench.mjs`"));
        assert!(message.contains("scenario id `site_build`"));
        assert!(message.contains("phase `iteration`"));
        assert!(message.contains("iteration 1"));
        assert!(message.contains("artifact key `visual_comparison_dir`"));

        results.scenarios[0].runs.as_mut().unwrap()[0]
            .artifacts
            .clear();
        validate_artifact_paths(&results, Some("studio-bfb"))
            .expect("valid artifact handling should remain unchanged");
    }
}
