use std::{collections::HashMap, fs};

use crate::test_support::with_isolated_home;
use homeboy::core::component::ScopedExtensionConfig;
use homeboy::core::rig::{self, ComponentSpec, RigSpec};

use super::test_fixture::{write_trace_extension, write_trace_rig, TRACE_FIXTURE_EXTENSION_ID};
use super::workload::trace_workload_scenario_id;
use super::*;

fn set_trace_rig_resources(home: &tempfile::TempDir, rig_id: &str, resources: serde_json::Value) {
    let rig_path = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("rigs")
        .join(format!("{rig_id}.json"));
    let mut rig_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&rig_path).expect("read rig")).expect("parse rig");
    rig_json["resources"] = resources;
    fs::write(
        &rig_path,
        serde_json::to_string(&rig_json).expect("serialize rig"),
    )
    .expect("write rig");
}

fn trace_args_for_rig(rig_id: &str, component_id: &str, scenario_id: &str) -> TraceArgs {
    TraceArgs {
        comp: PositionalComponentArgs {
            component: Some(component_id.to_string()),
            path: None,
        },
        component_arg: None,
        scenario: Some(scenario_id.to_string()),
        scenario_arg: None,
        compare_after: None,
        baseline_target: None,
        candidate: None,
        rig: Some(rig_id.to_string()),
        profile: None,
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
        checkout_provenance: None,
    }
}

#[test]
fn rig_trace_run_fails_fast_on_active_default_namespace_resource_lease() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let previous = std::env::var("HOMEBOY_TRACE_RESOURCE_NAMESPACE").ok();
        std::env::remove_var("HOMEBOY_TRACE_RESOURCE_NAMESPACE");
        let active_component = tempfile::TempDir::new().expect("active component");
        let blocked_component = tempfile::TempDir::new().expect("blocked component");
        write_trace_rig(home, "studio-rig", "studio", active_component.path());
        write_trace_rig(home, "studio-alt", "studio", blocked_component.path());
        let resources = serde_json::json!({
            "exclusive": ["wp-codebox-preview:${env.HOMEBOY_TRACE_RESOURCE_NAMESPACE}"]
        });
        set_trace_rig_resources(home, "studio-rig", resources.clone());
        set_trace_rig_resources(home, "studio-alt", resources);

        let active_spec = rig::load("studio-rig").expect("load active rig");
        let _lease = rig::lease::acquire_active_run_lease(&active_spec, "trace")
            .expect("acquire active trace lease")
            .expect("resourceful trace rig leases");

        let error = match run(
            trace_args_for_rig("studio-alt", "studio", "studio-app-create-site"),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("trace should fail before running with conflicting rig resources"),
            Err(error) => error,
        };

        match previous {
            Some(value) => std::env::set_var("HOMEBOY_TRACE_RESOURCE_NAMESPACE", value),
            None => std::env::remove_var("HOMEBOY_TRACE_RESOURCE_NAMESPACE"),
        }

        assert_eq!(
            error.code,
            homeboy::core::error::ErrorCode::RigResourceConflict
        );
        assert!(error.message.contains("studio-alt"));
        assert!(error.message.contains("studio-rig"));
        assert!(error.message.contains("wp-codebox-preview:<default>"));
        assert!(error.message.contains("trace"));
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("namespace") && hint.message.contains("port range")));
    });
}

#[test]
fn rig_component_path_and_trace_env_are_threaded() {
    let component_dir = tempfile::TempDir::new().expect("component dir");
    let mut components = HashMap::new();
    let mut extensions = HashMap::new();
    extensions.insert(
        "trace-extension".to_string(),
        ScopedExtensionConfig::default(),
    );
    components.insert(
        "studio".to_string(),
        ComponentSpec {
            path: component_dir.path().to_string_lossy().to_string(),
            checkout_root: None,
            remote_url: Some("https://github.com/Automattic/studio".to_string()),
            triage_remote_url: None,
            stack: None,
            branch: None,
            r#ref: None,
            extensions: Some(extensions),
        },
    );
    let spec = RigSpec {
        id: "studio-rig".to_string(),
        components,
        ..serde_json::from_str(r#"{"id":"studio-rig"}"#).unwrap()
    };
    let path = rig_component_path(&spec, "studio").expect("path resolves");
    assert_eq!(path, component_dir.path().to_string_lossy());
    let component = rig_component_for_trace(&spec, "studio").expect("component resolves");
    assert_eq!(component.id, "studio");
    assert_eq!(component.local_path, path);
    assert!(component.extensions.is_some());
}

#[test]
fn rig_component_for_trace_synthesizes_trace_workload_extensions() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
                "id": "studio",
                "components": {
                    "studio": { "path": "/tmp/studio" }
                },
                "trace_workloads": {
                    "trace-fixture": [{ "path": "/tmp/create-site.trace.mjs" }]
                }
            }"#,
    )
    .expect("parse rig spec");

    let component = rig_component_for_trace(&rig_spec, "studio").expect("component");

    assert!(component
        .extensions
        .as_ref()
        .expect("extensions")
        .contains_key(TRACE_FIXTURE_EXTENSION_ID));
}

#[test]
fn rig_trace_list_uses_rig_default_component_and_workloads() {
    let component_dir = tempfile::TempDir::new().expect("component dir");
    let rig_spec: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "studio-rig",
            "components": {{
                "studio": {{ "path": "{}" }}
            }},
            "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                {{ "path": "${{components.studio.path}}/studio-app-create-site.trace.mjs" }},
                {{ "path": "${{components.studio.path}}/studio-list-sites.trace.mjs" }}
            ] }}
        }}"#,
        component_dir.path().display()
    ))
    .expect("parse rig");

    let component_id = resolve_component_id(
        &PositionalComponentArgs {
            component: None,
            path: None,
        },
        Some(&rig_spec),
    )
    .expect("default component");
    let component = rig_component_for_trace(&rig_spec, &component_id).expect("component");
    let workloads = rig::workloads_for_extension(
        &rig_spec,
        rig::RigWorkloadKind::Trace,
        None,
        TRACE_FIXTURE_EXTENSION_ID,
    );
    let scenarios = workloads
        .iter()
        .map(|path| trace_workload_scenario_id(&path.to_string_lossy()))
        .collect::<Vec<_>>();

    assert_eq!(component_id, "studio");
    assert_eq!(component.id, "studio");
    assert_eq!(workloads.len(), 2);
    assert_eq!(scenarios[0], "studio-app-create-site");
    let expected_source = format!(
        "{}/studio-app-create-site.trace.mjs",
        component_dir.path().display()
    );
    assert_eq!(workloads[0].to_string_lossy(), expected_source);
}

#[test]
fn rig_trace_workload_expansion_warns_when_executed_path_differs_from_rig_root() {
    let rig_root = tempfile::TempDir::new().expect("rig root");
    let component_dir = tempfile::TempDir::new().expect("component dir");
    let trace_path = component_dir
        .path()
        .join("bench/ece-product-page-waterfall.trace.mjs");
    fs::create_dir_all(trace_path.parent().expect("trace parent")).expect("trace dir");
    fs::write(&trace_path, "export default {};").expect("trace workload");
    let rig_spec: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "wc-stripe-trace",
            "components": {{
                "stripe": {{ "path": "{}" }}
            }},
            "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                {{ "path": "${{components.stripe.path}}/bench/ece-product-page-waterfall.trace.mjs" }}
            ] }}
        }}"#,
        component_dir.path().display()
    ))
    .expect("parse rig");
    let context = TraceRigContext {
        rig_spec,
        rig_package_root: Some(rig_root.path().to_path_buf()),
        rig_config_root: None,
    };

    let warnings = rig_owned_trace_workload_expansion_warnings(
        Some(&context),
        Some(TRACE_FIXTURE_EXTENSION_ID),
    );

    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("wc-stripe-trace"));
    assert!(warnings[0]
        .contains("${components.stripe.path}/bench/ece-product-page-waterfall.trace.mjs"));
    assert!(warnings[0].contains(&format!("Rig source root: `{}`", rig_root.path().display())));
    assert!(warnings[0].contains(&format!(
        "Executed extra workload path: `{}`",
        trace_path.display()
    )));
    assert!(warnings[0].contains("refresh/reinstall the rig package"));
}

#[test]
fn rig_trace_workload_expansion_is_quiet_inside_rig_root() {
    let rig_root = tempfile::TempDir::new().expect("rig root");
    let rig_spec: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "package-owned-trace",
            "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                {{ "path": "${{package.root}}/bench/create-site.trace.mjs" }}
            ] }}
        }}"#
    ))
    .expect("parse rig");
    let context = TraceRigContext {
        rig_spec,
        rig_package_root: Some(rig_root.path().to_path_buf()),
        rig_config_root: None,
    };

    assert!(rig_owned_trace_workload_expansion_warnings(
        Some(&context),
        Some(TRACE_FIXTURE_EXTENSION_ID),
    )
    .is_empty());
}

#[test]
fn rig_trace_list_uses_scoped_workload_preflight() {
    let spec: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "studio-rig",
            "components": {{
                "studio": {{ "path": "/tmp/studio" }}
            }},
            "pipeline": {{
                "check": [
                    {{
                        "kind": "check",
                        "label": "desktop app packaged",
                        "groups": ["desktop-app"],
                        "command": "true"
                    }},
                    {{
                        "kind": "check",
                        "label": "unrelated cli symlink",
                        "groups": ["cli-dev-copy"],
                        "command": "false"
                    }}
                ]
            }},
            "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                {{
                    "path": "${{components.studio.path}}/studio-app-create-site.trace.mjs",
                    "check_groups": ["desktop-app"]
                }}
            ] }}
        }}"#
    ))
    .expect("parse rig");

    run_rig_workload_preflight(
        &spec,
        Some(TRACE_FIXTURE_EXTENSION_ID),
        rig::RigWorkloadKind::Trace,
    )
    .expect("scoped workload preflight should bypass unrelated failed check");

    assert!(run_rig_workload_preflight(&spec, None, rig::RigWorkloadKind::Trace).is_err());
}
