use std::collections::HashMap;

use crate::core::extension::trace::{TraceProbeConfig, TraceSpanMetadata};

use super::{
    TraceDependencySpec, TraceGuardrailSpec, TracePublicPreviewSpec, TraceVariantSpec, WorkloadSpec,
};

impl WorkloadSpec {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn public_preview(&self) -> Option<&TracePublicPreviewSpec> {
        self.public_preview.as_ref()
    }

    pub fn check_groups(&self) -> Option<&[String]> {
        self.check_groups.as_deref()
    }

    pub fn port_range_size(&self) -> Option<u16> {
        self.port_range_size
    }

    pub fn named_leases(&self) -> &[String] {
        &self.named_leases
    }

    pub fn trace_phase_preset(&self, name: &str) -> Option<&[String]> {
        self.trace
            .trace_phase_presets
            .get(name)
            .map(|phases| phases.as_slice())
    }

    pub fn trace_span_metadata(&self) -> &HashMap<String, TraceSpanMetadata> {
        &self.trace.trace_span_metadata
    }

    pub fn trace_default_phase_preset(&self) -> Option<&str> {
        self.trace.trace_default_phase_preset.as_deref()
    }

    pub fn trace_variants(&self) -> &HashMap<String, TraceVariantSpec> {
        &self.trace_variants
    }

    pub fn trace_guardrails(&self) -> &[TraceGuardrailSpec] {
        &self.trace_guardrails
    }

    pub fn trace_probes(&self) -> &[TraceProbeConfig] {
        &self.trace_probes
    }

    pub fn trace_dependencies(&self) -> &[TraceDependencySpec] {
        &self.dependencies
    }

    pub fn runner_capabilities(&self) -> &[String] {
        &self.runner_capabilities
    }
}
