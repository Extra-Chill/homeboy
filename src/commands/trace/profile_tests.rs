use std::fs;

use crate::test_support::with_isolated_home;

use super::test_fixture::{write_trace_extension, TRACE_FIXTURE_EXTENSION_ID};
use super::*;

fn trace_args_for_profile(profile: &str) -> TraceArgs {
    TraceArgs {
        comp: PositionalComponentArgs {
            component: None,
            path: None,
        },
        component_arg: None,
        scenario: None,
        scenario_arg: None,
        compare_after: None,
        baseline_target: None,
        candidate: None,
        rig: None,
        profile: Some(profile.to_string()),
        profiles: false,
        setting_args: SettingArgs::default(),
        json_summary: false,
        report: None,
        experiment: None,
        repeat: 1,
        aggregate: None,
        schedule: TraceSchedule::Grouped,
        focus_spans: Vec::new(),
        metric_guardrails: Vec::new(),
        spans: Vec::new(),
        phases: Vec::new(),
        attachments: Vec::new(),
        phase_preset: None,
        baseline_args: BaselineArgs::default(),
        regression_threshold: extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
        regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
        overlays: Vec::new(),
        variants: Vec::new(),
        matrix: TraceVariantMatrixMode::None,
        axes: Vec::new(),
        matrix_env: Vec::new(),
        output_dir: None,
        keep_overlay: false,
        stale: false,
        force: false,
        canonical: false,
        allow_local_toolchain: true,
    }
}

fn write_trace_rig_with_profile(
    home: &tempfile::TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{ "path": "{}" }}
                    }},
                    "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                        {{ "path": "${{components.{component_id}.path}}/close-window-running-site.trace.mjs" }}
                    ] }},
                    "trace_profiles": {{
                        "studio-window-close": {{
                            "component": "{component_id}",
                            "scenario": "close-window-running-site",
                            "settings": {{
                                "window_title": "Studio",
                                "retry_count": 2
                            }}
                        }}
                    }}
                }}"#,
            path.display()
        ),
    )
    .expect("write rig");
}

#[test]
fn trace_profile_resolves_to_normal_trace_run() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_profile(home, "studio-rig", "studio", component_dir.path());

        let ((output, _artifact_output), exit_code) =
            run_outputs(trace_args_for_profile("studio-window-close"))
                .expect("profile trace should run");

        assert_eq!(exit_code, 0);
        let TraceCommandOutput::Run(run) = output else {
            panic!("expected run output");
        };
        let profile = run.profile.expect("resolved profile metadata");
        assert_eq!(profile.id, "studio-window-close");
        assert_eq!(profile.rig_id.as_deref(), Some("studio-rig"));
        assert_eq!(profile.component, "studio");
        assert_eq!(profile.scenario, "close-window-running-site");
        assert_eq!(
            profile.settings.get("window_title"),
            Some(&serde_json::Value::String("Studio".to_string()))
        );
        assert_eq!(
            profile.settings.get("retry_count"),
            Some(&serde_json::json!(2))
        );
    });
}

#[test]
fn trace_profile_cli_fields_override_profile_fields() {
    with_isolated_home(|home| {
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_profile(home, "studio-rig", "studio", component_dir.path());
        let mut args = trace_args_for_profile("studio-window-close");
        args.scenario = Some("close-window-retry".to_string());
        args.setting_args
            .setting
            .push(("window_title".to_string(), "Studio Dev".to_string()));

        resolve_trace_profile_args(&mut args).expect("profile resolves");

        assert_eq!(args.comp.component.as_deref(), Some("studio"));
        assert_eq!(args.scenario.as_deref(), Some("close-window-retry"));
        assert_eq!(args.rig.as_deref(), Some("studio-rig"));
        assert_eq!(
            args.setting_args.setting,
            vec![
                ("window_title".to_string(), "Studio".to_string()),
                ("window_title".to_string(), "Studio Dev".to_string())
            ]
        );
    });
}

#[test]
fn trace_list_profiles_lists_rig_profiles() {
    with_isolated_home(|home| {
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_profile(home, "studio-rig", "studio", component_dir.path());

        let ((output, _artifact_output), exit_code) = run_outputs(TraceArgs {
            comp: PositionalComponentArgs {
                component: Some("list".to_string()),
                path: None,
            },
            component_arg: None,
            scenario: None,
            scenario_arg: None,
            compare_after: None,
            baseline_target: None,
            candidate: None,
            rig: None,
            profile: None,
            profiles: true,
            setting_args: SettingArgs::default(),
            json_summary: false,
            report: None,
            experiment: None,
            repeat: 1,
            aggregate: None,
            schedule: TraceSchedule::Grouped,
            focus_spans: Vec::new(),
            metric_guardrails: Vec::new(),
            spans: Vec::new(),
            phases: Vec::new(),
            attachments: Vec::new(),
            phase_preset: None,
            baseline_args: BaselineArgs::default(),
            regression_threshold: extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
            regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
            overlays: Vec::new(),
            variants: Vec::new(),
            matrix: TraceVariantMatrixMode::None,
            axes: Vec::new(),
            matrix_env: Vec::new(),
            output_dir: None,
            keep_overlay: false,
            stale: false,
            force: false,
            canonical: false,
            allow_local_toolchain: true,
        })
        .expect("profile list should run");

        assert_eq!(exit_code, 0);
        let TraceCommandOutput::List(list) = output else {
            panic!("expected list output");
        };
        assert_eq!(list.command, "trace.list.profiles");
        assert_eq!(list.count, 1);
        assert_eq!(list.profiles[0].id, "studio-window-close");
        assert_eq!(list.profiles[0].rig_id, "studio-rig");
        assert_eq!(list.profiles[0].component.as_deref(), Some("studio"));
        assert_eq!(
            list.profiles[0].scenario.as_deref(),
            Some("close-window-running-site")
        );
    });
}

#[test]
fn trace_profile_public_preview_overrides_workload_preview() {
    with_isolated_home(|home| {
        let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
        fs::create_dir_all(&rig_dir).expect("mkdir rigs");
        let component_dir = tempfile::TempDir::new().expect("component dir");
        fs::write(
            rig_dir.join("studio-rig.json"),
            format!(
                r#"{{
                    "components": {{
                        "studio": {{ "path": "{}" }}
                    }},
                    "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                        {{
                            "path": "${{components.studio.path}}/wallet.trace.mjs",
                            "public_preview": {{
                                "local_origin": "http://127.0.0.1:8080",
                                "public_origin": "https://workload.example.test",
                                "require_https": true
                            }}
                        }}
                    ] }},
                    "trace_profiles": {{
                        "wallet": {{
                            "component": "studio",
                            "scenario": "wallet",
                            "public_preview": {{
                                "local_origin": "http://127.0.0.1:9090",
                                "public_origin": "https://profile.example.test",
                                "require_https": true,
                                "provider": "fixture"
                            }}
                        }}
                    }}
                }}"#,
                component_dir.path().display()
            ),
        )
        .expect("write rig");

        let mut args = trace_args_for_profile("wallet");
        resolve_trace_profile_args(&mut args).expect("profile resolves");
        let context = load_rig_context(args.rig.as_deref()).expect("rig context");
        let preview = trace_public_preview_for_args(
            &args,
            context.as_ref(),
            Some(TRACE_FIXTURE_EXTENSION_ID),
        )
        .expect("preview resolves")
        .expect("preview present");

        assert_eq!(preview.local_origin, "http://127.0.0.1:9090");
        assert_eq!(
            preview.public_origin.as_deref(),
            Some("https://profile.example.test")
        );
        assert_eq!(preview.provider.as_deref(), Some("fixture"));
        assert!(preview.require_https);
    });
}
