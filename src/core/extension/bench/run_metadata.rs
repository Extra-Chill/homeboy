use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::core::component::Component;
use crate::core::extension::bench::parsing::{
    BenchResults, BenchRunMetadata, BenchRunnerMetadata, BenchScenario, BenchWorkloadMetadata,
};
use crate::core::extension::bench::run::BenchRunWorkflowArgs;
use crate::core::extension::ExtensionExecutionContext;

pub(crate) fn stamp_run_metadata(
    results: &mut BenchResults,
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    started_at: &str,
) {
    let mut workloads = workload_metadata(&results.scenarios, component, &args.extra_workloads);
    workloads.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.path.cmp(&b.path)));

    results.run_metadata = Some(BenchRunMetadata {
        homeboy_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        started_at: started_at.to_string(),
        shared_state: args
            .shared_state
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        iterations: args.iterations,
        execution: args.execution,
        warmup_iterations: bench_warmup_iterations(),
        selected_scenarios: args.scenario_ids.clone(),
        env_overrides: bench_env_overrides(),
        workloads,
        runner: Some(BenchRunnerMetadata {
            extension: execution_context.extension_id.clone(),
            path: execution_context
                .extension_path
                .to_string_lossy()
                .to_string(),
            source_revision: source_revision_at(&execution_context.extension_path),
        }),
        diagnostics: Vec::new(),
    });
}

fn workload_metadata(
    scenarios: &[BenchScenario],
    component: &Component,
    extra_workloads: &[PathBuf],
) -> Vec<BenchWorkloadMetadata> {
    let mut workloads = Vec::new();
    let mut seen_paths = BTreeSet::new();

    for scenario in scenarios {
        let resolved = scenario
            .file
            .as_deref()
            .map(|path| resolve_workload_path(path, component));
        if let Some(path) = &resolved {
            seen_paths.insert(path.to_string_lossy().to_string());
        }
        workloads.push(BenchWorkloadMetadata {
            id: scenario.id.clone(),
            source: scenario.source.clone(),
            path: resolved
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            sha256: resolved.as_deref().and_then(sha256_file),
        });
    }

    for path in extra_workloads {
        let path_string = path.to_string_lossy().to_string();
        if !seen_paths.insert(path_string.clone()) {
            continue;
        }
        workloads.push(BenchWorkloadMetadata {
            id: path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("extra-workload")
                .to_string(),
            source: Some("rig".to_string()),
            path: Some(path_string),
            sha256: sha256_file(path),
        });
    }

    workloads
}

fn resolve_workload_path(path: &str, component: &Component) -> PathBuf {
    let workload_path = PathBuf::from(path);
    if workload_path.is_absolute() {
        workload_path
    } else {
        PathBuf::from(&component.local_path).join(workload_path)
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let hash = Sha256::digest(&bytes);
    Some(hash.iter().map(|byte| format!("{:02x}", byte)).collect())
}

fn bench_warmup_iterations() -> Option<u64> {
    std::env::var("HOMEBOY_BENCH_WARMUP_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
}

fn bench_env_overrides() -> BTreeMap<String, String> {
    bench_env_overrides_from_iter(std::env::vars())
}

fn bench_env_overrides_from_iter<I, K, V>(vars: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    vars.into_iter()
        .filter_map(|(key, value)| {
            let key = key.into();
            if key.starts_with("HOMEBOY_BENCH_") && !is_secret_like_env_key(&key) {
                Some((key, value.into()))
            } else {
                None
            }
        })
        .collect()
}

fn is_secret_like_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "AUTH",
        "API_KEY",
        "PRIVATE_KEY",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn source_revision_at(path: &Path) -> Option<String> {
    crate::core::git::short_head_revision_at(path).or_else(|| {
        std::fs::read_to_string(path.join(".source-revision"))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::baseline::BaselineFlags;
    use crate::core::engine::invocation::InvocationRequirements;
    use crate::core::extension::bench::parsing::{self, BenchRunExecution};
    use crate::core::extension::ExtensionCapability;

    #[test]
    fn run_metadata_captures_reproducible_bench_context() {
        let component_dir = tempfile::TempDir::new().expect("component dir");
        let workload_dir = component_dir.path().join("tests/bench");
        std::fs::create_dir_all(&workload_dir).expect("workload dir");
        let workload = workload_dir.join("boot.rs");
        std::fs::write(&workload, "fn main() {}\n").expect("workload file");
        let extension_dir = tempfile::TempDir::new().expect("extension dir");

        let component = Component {
            id: "homeboy".to_string(),
            local_path: component_dir.path().to_string_lossy().to_string(),
            ..Component::default()
        };
        let execution_context = ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Bench,
            extension_id: "rust".to_string(),
            extension_path: extension_dir.path().to_path_buf(),
            script_path: "bench-runner.sh".to_string(),
            settings: Vec::new(),
        };
        let args = BenchRunWorkflowArgs {
            component_label: "homeboy".to_string(),
            component_id: "homeboy".to_string(),
            path_override: None,
            settings: Vec::new(),
            settings_json: Vec::new(),
            iterations: 7,
            warmup_iterations: None,
            execution: BenchRunExecution {
                runs: 3,
                concurrency: 2,
            },
            baseline_flags: BaselineFlags {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold_percent: 5.0,
            json_summary: false,
            ci_env: Vec::new(),
            passthrough_args: Vec::new(),
            scenario_ids: vec!["boot".to_string()],
            rig_id: Some("studio".to_string()),
            shared_state: Some(component_dir.path().join("shared")),
            extra_workloads: Vec::new(),
            invocation_requirements: InvocationRequirements::default(),
        };
        let mut results = BenchResults {
            component_id: "homeboy".to_string(),
            iterations: 7,
            run_metadata: None,
            metadata: BTreeMap::new(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: BTreeMap::new(),
            diagnostics: Vec::new(),
            phase_events: Vec::new(),
            phase_summaries: Vec::new(),
            failure_classification: None,
            budget_findings: Vec::new(),
            scenarios: vec![BenchScenario {
                id: "boot".to_string(),
                file: Some("tests/bench/boot.rs".to_string()),
                source: Some("in_tree".to_string()),
                default_iterations: None,
                tags: Vec::new(),
                iterations: 7,
                metrics: parsing::BenchMetrics {
                    values: BTreeMap::new(),
                    distributions: BTreeMap::new(),
                },
                metric_groups: BTreeMap::new(),
                timeline: Vec::new(),
                span_definitions: Vec::new(),
                span_results: Vec::new(),
                gates: Vec::new(),
                gate_results: Vec::new(),
                metadata: BTreeMap::new(),
                passed: true,
                memory: None,
                artifacts: BTreeMap::new(),
                diagnostics: Vec::new(),
                runs: None,
                runs_summary: None,
            }],
            metric_policies: BTreeMap::new(),
            metric_policy_presets: BTreeMap::new(),
        };

        stamp_run_metadata(
            &mut results,
            &execution_context,
            &component,
            &args,
            "2026-04-28T00:00:00Z",
        );

        let metadata = results.run_metadata.expect("metadata stamped");
        assert_eq!(
            metadata.homeboy_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(metadata.started_at, "2026-04-28T00:00:00Z");
        assert_eq!(metadata.iterations, 7);
        assert_eq!(metadata.execution.runs, 3);
        assert_eq!(metadata.execution.concurrency, 2);
        assert_eq!(metadata.selected_scenarios, vec!["boot".to_string()]);
        assert_eq!(metadata.runner.as_ref().unwrap().extension, "rust");
        assert_eq!(metadata.workloads.len(), 1);
        assert_eq!(metadata.workloads[0].id, "boot");
        assert_eq!(metadata.workloads[0].source.as_deref(), Some("in_tree"));
        assert_eq!(
            metadata.workloads[0].path.as_deref(),
            Some(workload.to_string_lossy().as_ref())
        );
        assert_eq!(metadata.workloads[0].sha256.as_ref().unwrap().len(), 64);
    }

    #[test]
    fn bench_env_overrides_are_allow_listed_and_secret_safe() {
        let vars = vec![
            ("HOMEBOY_BENCH_WARMUP_ITERATIONS", "0"),
            ("HOMEBOY_BENCH_PROFILE", "cold"),
            ("HOMEBOY_BENCH_TOKEN", "secret"),
            ("HOMEBOY_BENCH_API_KEY", "secret"),
            ("DATABASE_URL", "postgres://user:pass@example/db"),
        ];

        let captured = bench_env_overrides_from_iter(vars);

        assert_eq!(
            captured.get("HOMEBOY_BENCH_WARMUP_ITERATIONS"),
            Some(&"0".to_string())
        );
        assert_eq!(
            captured.get("HOMEBOY_BENCH_PROFILE"),
            Some(&"cold".to_string())
        );
        assert!(!captured.contains_key("HOMEBOY_BENCH_TOKEN"));
        assert!(!captured.contains_key("HOMEBOY_BENCH_API_KEY"));
        assert!(!captured.contains_key("DATABASE_URL"));
    }
}
