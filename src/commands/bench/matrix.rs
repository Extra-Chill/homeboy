use std::path::PathBuf;

use homeboy::component::Component;
use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::engine::run_dir::RunDir;
use homeboy::extension::bench as extension_bench;
use homeboy::extension::bench::{BenchCommandOutput, BenchResults, BenchRunWorkflowArgs};
use homeboy::extension::ExtensionCapability;
use homeboy::rig::{self, BenchSpec, RigSpec, RigStateSnapshot};

use super::{bench_workloads_for_extension, BenchRunArgs, CmdResult};

struct RigBenchContext {
    id: String,
    spec: RigSpec,
    package_root: Option<PathBuf>,
    snapshot: RigStateSnapshot,
}

fn prepare_rig_bench_context(rig_id: &str) -> homeboy::Result<RigBenchContext> {
    let spec = rig::load(rig_id)?;
    let check_report = rig::run_check(&spec)?;
    if !check_report.success {
        return Err(homeboy::Error::rig_pipeline_failed(
            &spec.id,
            "check",
            "rig check failed; refusing to run bench against an unhealthy rig",
        ));
    }
    let snapshot = rig::snapshot_state(&spec);
    let package_root =
        rig::read_source_metadata(&spec.id).map(|metadata| PathBuf::from(metadata.package_path));
    Ok(RigBenchContext {
        id: spec.id.clone(),
        spec,
        package_root,
        snapshot,
    })
}

pub(super) fn bench_component_ids(bench: &BenchSpec) -> Vec<String> {
    if !bench.components.is_empty() {
        return bench.components.clone();
    }
    bench.default_component.iter().cloned().collect()
}

fn rig_bench_components(spec: &RigSpec) -> Vec<String> {
    spec.bench
        .as_ref()
        .map(bench_component_ids)
        .unwrap_or_default()
}

fn rig_component_path(spec: &RigSpec, component_id: &str) -> Option<String> {
    spec.components
        .get(component_id)
        .map(|component| rig::expand::expand_vars(spec, &component.path))
}

fn rig_component_for_bench(spec: &RigSpec, component_id: &str) -> Option<Component> {
    let rig_component = spec.components.get(component_id)?;
    let extensions = rig_component.extensions.clone()?;
    let mut component = Component {
        id: component_id.to_string(),
        local_path: rig::expand::expand_vars(spec, &rig_component.path),
        remote_url: rig_component.remote_url.clone(),
        extensions: Some(extensions),
        ..Component::default()
    };
    component.resolve_remote_path();
    Some(component)
}

fn component_shared_state(
    args: &BenchRunArgs,
    component_id: &str,
    matrix_len: usize,
) -> Option<PathBuf> {
    args.shared_state.as_ref().map(|path| {
        if matrix_len > 1 {
            path.join(component_id)
        } else {
            path.clone()
        }
    })
}

fn suffix_component_results(mut results: BenchResults, component_id: &str) -> BenchResults {
    for scenario in &mut results.scenarios {
        scenario.id = format!("{}:c{}", scenario.id, component_id);
    }
    results
}

fn merge_matrix_results(
    component_ids: &[String],
    outputs: &[BenchCommandOutput],
) -> Option<BenchResults> {
    let mut merged_scenarios = Vec::new();
    let mut component_id_seen: Option<String> = None;
    let mut iterations_seen: Option<u64> = None;
    let mut metric_policies_seen = std::collections::BTreeMap::new();

    for (component_id, output) in component_ids.iter().zip(outputs.iter()) {
        let Some(results) = output.results.clone() else {
            continue;
        };
        let suffixed = suffix_component_results(results, component_id);
        if component_id_seen.is_none() {
            component_id_seen = Some(suffixed.component_id.clone());
        }
        if iterations_seen.is_none() {
            iterations_seen = Some(suffixed.iterations);
        }
        for (key, policy) in suffixed.metric_policies {
            metric_policies_seen.entry(key).or_insert(policy);
        }
        merged_scenarios.extend(suffixed.scenarios);
    }

    if merged_scenarios.is_empty() && component_id_seen.is_none() {
        None
    } else {
        Some(BenchResults {
            component_id: component_ids.join(","),
            iterations: iterations_seen.unwrap_or(0),
            scenarios: merged_scenarios,
            metric_policies: metric_policies_seen,
        })
    }
}

pub(super) fn run_single_rig(
    args: &BenchRunArgs,
    passthrough_args: &[String],
    rig_id: String,
) -> CmdResult<BenchCommandOutput> {
    let context = prepare_rig_bench_context(&rig_id)?;
    let matrix_components = if let Some(explicit) = args.comp.id() {
        vec![explicit.to_string()]
    } else {
        rig_bench_components(&context.spec)
    };

    if matrix_components.len() <= 1 {
        let component_override = matrix_components.first().cloned();
        let shared_state = component_override
            .as_deref()
            .and_then(|id| component_shared_state(args, id, matrix_components.len()));
        return run_component_with_rig_context(
            args,
            passthrough_args,
            Some(&context),
            component_override,
            shared_state,
        );
    }

    let mut outputs = Vec::with_capacity(matrix_components.len());
    let mut first_nonzero_exit: Option<i32> = None;

    for component_id in &matrix_components {
        let shared_state = component_shared_state(args, component_id, matrix_components.len());
        let (output, exit_code) = run_component_with_rig_context(
            args,
            passthrough_args,
            Some(&context),
            Some(component_id.clone()),
            shared_state,
        )?;
        if exit_code != 0 && first_nonzero_exit.is_none() {
            first_nonzero_exit = Some(exit_code);
        }
        outputs.push(output);
    }

    let exit_code = first_nonzero_exit.unwrap_or(0);
    let mut hints = Vec::new();
    for output in &outputs {
        if let Some(output_hints) = &output.hints {
            for hint in output_hints {
                if !hints.contains(hint) {
                    hints.push(hint.clone());
                }
            }
        }
    }

    Ok((
        BenchCommandOutput {
            passed: outputs.iter().all(|output| output.passed),
            status: if exit_code == 0 { "passed" } else { "failed" }.to_string(),
            component: matrix_components.join(","),
            exit_code,
            iterations: args.iterations,
            results: merge_matrix_results(&matrix_components, &outputs),
            baseline_comparison: None,
            hints: if hints.is_empty() { None } else { Some(hints) },
            rig_state: Some(context.snapshot),
        },
        exit_code,
    ))
}

pub(super) fn run_single(
    args: &BenchRunArgs,
    passthrough_args: &[String],
    rig_id_override: Option<String>,
) -> CmdResult<BenchCommandOutput> {
    let rig_context = match rig_id_override.as_deref() {
        None => None,
        Some(rig_id) => Some(prepare_rig_bench_context(rig_id)?),
    };
    run_component_with_rig_context(args, passthrough_args, rig_context.as_ref(), None, None)
}

fn run_component_with_rig_context(
    args: &BenchRunArgs,
    passthrough_args: &[String],
    rig_context: Option<&RigBenchContext>,
    component_override: Option<String>,
    shared_state_override: Option<PathBuf>,
) -> CmdResult<BenchCommandOutput> {
    let rig_spec = rig_context.map(|context| &context.spec);
    let rig_id = rig_context.map(|context| context.id.clone());
    let rig_snapshot = rig_context.map(|context| context.snapshot.clone());
    let default_component_id = rig_spec.and_then(|spec| {
        spec.bench
            .as_ref()
            .and_then(|bench| bench_component_ids(bench).into_iter().next())
    });

    let effective_id = match (component_override, args.comp.id(), default_component_id) {
        (Some(id), _, _) => id,
        (None, Some(id), _) => id.to_string(),
        (None, None, Some(default)) => default,
        (None, None, None) => args.comp.resolve_id()?,
    };

    let path_override = args
        .comp
        .path
        .clone()
        .or_else(|| rig_spec.and_then(|spec| rig_component_path(spec, &effective_id)));

    let component_override = rig_spec
        .as_ref()
        .and_then(|spec| rig_component_for_bench(spec, &effective_id));

    let ctx = execution_context::resolve_with_component(
        &ResolveOptions::with_capability_and_json(
            &effective_id,
            path_override.clone(),
            ExtensionCapability::Bench,
            args.setting_args.setting.clone(),
            args.setting_args.setting_json.clone(),
        ),
        component_override,
    )?;

    let run_dir = RunDir::create()?;

    let extra_workloads = rig_spec
        .as_ref()
        .and_then(|spec| {
            ctx.extension_id.as_deref().map(|id| {
                bench_workloads_for_extension(
                    spec,
                    rig_context.and_then(|context| context.package_root.as_deref()),
                    id,
                )
            })
        })
        .unwrap_or_default();

    let workflow = extension_bench::run_main_bench_workflow(
        &ctx.component,
        &ctx.source_path,
        BenchRunWorkflowArgs {
            component_label: effective_id.clone(),
            component_id: ctx.component_id.clone(),
            path_override,
            settings: ctx
                .settings
                .iter()
                .filter_map(|(k, v)| match v {
                    serde_json::Value::String(s) => Some((k.clone(), s.clone())),
                    _ => None,
                })
                .collect(),
            settings_json: ctx
                .settings
                .iter()
                .filter_map(|(k, v)| match v {
                    serde_json::Value::String(_) => None,
                    other => Some((k.clone(), other.clone())),
                })
                .collect(),
            iterations: args.iterations,
            runs: args.runs,
            baseline_flags: homeboy::engine::baseline::BaselineFlags {
                baseline: args.baseline_args.baseline,
                ignore_baseline: args.baseline_args.ignore_baseline,
                ratchet: args.baseline_args.ratchet,
            },
            regression_threshold_percent: args.regression_threshold,
            json_summary: args.json_summary,
            passthrough_args: passthrough_args.to_vec(),
            rig_id: rig_id.clone(),
            shared_state: shared_state_override.or_else(|| args.shared_state.clone()),
            concurrency: args.concurrency,
            extra_workloads,
        },
        &run_dir,
    )?;

    Ok(extension_bench::from_main_workflow_with_rig(
        workflow,
        rig_snapshot,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::{
        BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs,
    };
    use crate::test_support::with_isolated_home;
    use std::fs;

    fn write_bench_extension(home: &tempfile::TempDir, extension_id: &str) {
        let extension_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("extensions")
            .join(extension_id);
        fs::create_dir_all(&extension_dir).expect("mkdir extension");
        fs::write(
            extension_dir.join(format!("{}.json", extension_id)),
            r#"{
                "name": "Node.js",
                "version": "0.0.0",
                "bench": { "extension_script": "bench-runner.sh" }
            }"#,
        )
        .expect("write extension manifest");
    }

    #[test]
    fn rig_bench_components_prefers_matrix_over_default_component() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "mdi-substrates",
                "bench": {
                    "default_component": "legacy-default",
                    "components": ["mdi-sdi", "mdi-primary"]
                }
            }"#,
        )
        .expect("parse rig spec");

        assert_eq!(
            rig_bench_components(&rig_spec),
            vec!["mdi-sdi".to_string(), "mdi-primary".to_string()]
        );
    }

    #[test]
    fn rig_bench_components_falls_back_to_default_component() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "legacy",
                "bench": { "default_component": "homeboy" }
            }"#,
        )
        .expect("parse rig spec");

        assert_eq!(rig_bench_components(&rig_spec), vec!["homeboy".to_string()]);
    }

    #[test]
    fn component_shared_state_uses_subdirs_for_matrix_only() {
        let mut args = BenchRunArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            iterations: 1,
            runs: 1,
            shared_state: Some(PathBuf::from("/tmp/shared")),
            concurrency: 1,
            baseline_args: BaselineArgs::default(),
            regression_threshold: 5.0,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            _json: HiddenJsonArgs::default(),
            json_summary: false,
            rig: vec!["rig".to_string()],
            ignore_default_baseline: false,
        };

        assert_eq!(
            component_shared_state(&args, "mdi-primary", 3),
            Some(PathBuf::from("/tmp/shared/mdi-primary"))
        );
        assert_eq!(
            component_shared_state(&args, "mdi-primary", 1),
            Some(PathBuf::from("/tmp/shared"))
        );

        args.shared_state = None;
        assert_eq!(component_shared_state(&args, "mdi-primary", 3), None);
    }

    #[test]
    fn rig_component_for_bench_synthesizes_extension_config() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let rig_spec: RigSpec = serde_json::from_str(&format!(
            r#"{{
                "id": "studio",
                "components": {{
                    "studio": {{
                        "path": "{}",
                        "extensions": {{
                            "nodejs": {{
                                "settings": {{ "package_manager": "pnpm" }},
                                "workspace": "apps/studio"
                            }}
                        }}
                    }}
                }},
                "bench": {{ "default_component": "studio" }}
            }}"#,
            temp.path().display()
        ))
        .expect("parse rig spec");

        let component = rig_component_for_bench(&rig_spec, "studio")
            .expect("rig component with extensions should synthesize component");

        assert_eq!(component.id, "studio");
        assert_eq!(component.local_path, temp.path().to_string_lossy());
        let nodejs = component
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get("nodejs"))
            .expect("nodejs config preserved");
        assert_eq!(
            nodejs.settings.get("package_manager"),
            Some(&serde_json::json!("pnpm"))
        );
        assert_eq!(
            nodejs.settings.get("workspace"),
            Some(&serde_json::json!("apps/studio"))
        );
    }

    #[test]
    fn rig_component_for_bench_absent_extension_config_falls_back() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "legacy",
                "components": { "studio": { "path": "/tmp/studio" } },
                "bench": { "default_component": "studio" }
            }"#,
        )
        .expect("parse rig spec");

        assert!(rig_component_for_bench(&rig_spec, "studio").is_none());
        assert!(rig_component_for_bench(&rig_spec, "missing").is_none());
    }

    #[test]
    fn rig_component_extension_config_resolves_bench_context() {
        with_isolated_home(|home| {
            write_bench_extension(home, "nodejs");
            let temp = tempfile::TempDir::new().expect("component dir");
            let rig_spec: RigSpec = serde_json::from_str(&format!(
                r#"{{
                    "id": "studio",
                    "components": {{
                        "studio": {{
                            "path": "{}",
                            "extensions": {{
                                "nodejs": {{ "package_manager": "pnpm" }}
                            }}
                        }}
                    }},
                    "bench": {{ "default_component": "studio" }}
                }}"#,
                temp.path().display()
            ))
            .expect("parse rig spec");
            let component_override = rig_component_for_bench(&rig_spec, "studio");

            let ctx = execution_context::resolve_with_component(
                &ResolveOptions::with_capability_and_json(
                    "studio",
                    Some(temp.path().to_string_lossy().to_string()),
                    ExtensionCapability::Bench,
                    Vec::new(),
                    Vec::new(),
                ),
                component_override,
            )
            .expect("rig-owned extension config resolves bench context");

            assert_eq!(ctx.component_id, "studio");
            assert_eq!(ctx.extension_id.as_deref(), Some("nodejs"));
            assert!(ctx
                .settings
                .iter()
                .any(|(key, value)| key == "package_manager" && value == "pnpm"));
        });
    }

    #[test]
    fn missing_rig_extension_config_keeps_clear_error() {
        let temp = tempfile::TempDir::new().expect("component dir");
        let err = execution_context::resolve_with_component(
            &ResolveOptions::with_capability_and_json(
                "studio",
                Some(temp.path().to_string_lossy().to_string()),
                ExtensionCapability::Bench,
                Vec::new(),
                Vec::new(),
            ),
            Some(Component {
                id: "studio".to_string(),
                local_path: temp.path().to_string_lossy().to_string(),
                ..Component::default()
            }),
        )
        .expect_err("component without extensions should fail clearly");

        let message = err.to_string();
        assert!(
            message.contains("has no extensions configured"),
            "expected missing-extension error, got: {}",
            message
        );
    }

    fn bench_results(component_id: &str, scenario_id: &str, p95: f64) -> BenchResults {
        serde_json::from_value(serde_json::json!({
            "component_id": component_id,
            "iterations": 10,
            "scenarios": [
                {
                    "id": scenario_id,
                    "iterations": 10,
                    "metrics": { "p95_ms": p95 }
                }
            ],
            "metric_policies": {
                "p95_ms": { "direction": "lower_is_better" }
            }
        }))
        .expect("bench results")
    }

    fn bench_output(component: &str, results: Option<BenchResults>) -> BenchCommandOutput {
        BenchCommandOutput {
            passed: true,
            status: "passed".to_string(),
            component: component.to_string(),
            exit_code: 0,
            iterations: 10,
            results,
            baseline_comparison: None,
            hints: None,
            rig_state: None,
        }
    }

    #[test]
    fn merge_matrix_results_suffixes_scenarios_by_component() {
        let component_ids = vec!["mdi-sdi".to_string(), "mdi-primary".to_string()];
        let outputs = vec![
            bench_output("mdi-sdi", Some(bench_results("mdi-sdi", "cold-boot", 42.0))),
            bench_output(
                "mdi-primary",
                Some(bench_results("mdi-primary", "cold-boot", 50.0)),
            ),
        ];

        let merged = merge_matrix_results(&component_ids, &outputs).expect("merged results");
        assert_eq!(merged.component_id, "mdi-sdi,mdi-primary");
        assert_eq!(merged.iterations, 10);
        assert_eq!(merged.scenarios.len(), 2);
        assert_eq!(merged.scenarios[0].id, "cold-boot:cmdi-sdi");
        assert_eq!(merged.scenarios[1].id, "cold-boot:cmdi-primary");
        assert!(merged.metric_policies.contains_key("p95_ms"));
    }

    #[test]
    fn merge_matrix_results_skips_components_without_parseable_results() {
        let component_ids = vec!["a".to_string(), "b".to_string()];
        let outputs = vec![
            bench_output("a", None),
            bench_output("b", Some(bench_results("b", "boot", 10.0))),
        ];

        let merged = merge_matrix_results(&component_ids, &outputs).expect("merged results");
        assert_eq!(merged.scenarios.len(), 1);
        assert_eq!(merged.scenarios[0].id, "boot:cb");
    }
}
