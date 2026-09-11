//! Shared fixtures for extension trace tests.

use crate::extension::resolve::ExtensionExecutionContext;
use crate::extension::trace::canonicality::TraceCanonicalPolicy;
use crate::extension::trace::run::{TraceRunWorkflowArgs, TraceRunnerInputs};
use homeboy_core::component::Component;
use homeboy_engine_primitives::baseline::BaselineFlags;
use homeboy_extension_contract::ExtensionCapability;

pub(crate) fn write_trace_extension(
    home: &std::path::Path,
    component: &Component,
) -> ExtensionExecutionContext {
    let extension_id = "fixture-extension";
    let extension_dir = home.join(".config/homeboy/extensions").join(extension_id);
    std::fs::create_dir_all(&extension_dir).expect("extension dir");
    std::fs::write(
        extension_dir.join(format!("{extension_id}.json")),
        serde_json::json!({
            "name": "Fixture Extension",
            "version": "0.0.0",
            "trace": {
                "extension_script": "trace.js",
                "toolchain_provenance": [
                    {
                        "id": "fixture-toolchain",
                        "label": "Fixture Toolchain",
                        "env_keys": ["FIXTURE_TOOLCHAIN_BIN"]
                    }
                ]
            }
        })
        .to_string(),
    )
    .expect("extension manifest");
    std::fs::write(extension_dir.join("trace.js"), "#!/usr/bin/env node\n").unwrap();

    ExtensionExecutionContext {
        component: component.clone(),
        capability: ExtensionCapability::Trace,
        extension_id: extension_id.to_string(),
        extension_path: extension_dir,
        script_path: "trace.js".to_string(),
        settings: Vec::new(),
        accepted_setting_keys: Vec::new(),
    }
}

pub(crate) fn test_run_args(path: &std::path::Path) -> TraceRunWorkflowArgs {
    TraceRunWorkflowArgs {
        component_label: "example".to_string(),
        component_id: "example".to_string(),
        path_override: Some(path.to_string_lossy().to_string()),
        settings: Vec::new(),
        runner_inputs: TraceRunnerInputs::default(),
        scenario_id: "missing".to_string(),
        json_summary: false,
        rig_id: None,
        overlays: Vec::new(),
        keep_overlay: false,
        span_definitions: Vec::new(),
        baseline_flags: BaselineFlags {
            baseline: false,
            ignore_baseline: true,
            ratchet: false,
        },
        regression_threshold_percent:
            crate::extension::trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
        regression_min_delta_ms: crate::extension::trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
        canonical_policy: TraceCanonicalPolicy::Development,
        checkout_provenance: None,
    }
}
