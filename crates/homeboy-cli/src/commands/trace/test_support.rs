//! Shared fixtures for `homeboy trace` tests.

use super::test_fixture::{write_trace_extension, write_trace_rig, TRACE_FIXTURE_EXTENSION_ID};
use super::workload::trace_workload_scenario_id;
use super::*;
use crate::test_support::with_isolated_home;
use homeboy::core::component::ScopedExtensionConfig;
use homeboy::rig::{self, ComponentSpec, RigSpec};
use std::{collections::HashMap, fs};

pub(super) fn trace_args_for_rig(rig_id: &str, component_id: &str, scenario_id: &str) -> TraceArgs {
    TraceArgs {
        command: None,
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
        secret_env: Vec::new(),
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
        visual_compare: false,
        visual_artifacts_dir: None,
        visual_compare_provider: None,
        visual_provider_args: Vec::new(),
        visual_threshold: None,
        keep_overlay: false,
        stale: false,
        force: false,
        canonical: false,
        allow_local_toolchain: true,
        checkout_provenance: None,
    }
}
