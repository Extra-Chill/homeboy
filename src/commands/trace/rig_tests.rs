use std::collections::HashMap;

use homeboy::core::component::ScopedExtensionConfig;
use homeboy::core::rig::{self, ComponentSpec, RigSpec};

use super::test_fixture::TRACE_FIXTURE_EXTENSION_ID;
use super::workload::trace_workload_scenario_id;
use super::*;

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
