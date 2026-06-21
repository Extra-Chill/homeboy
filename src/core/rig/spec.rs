//! Rig spec types — the JSON schema on disk.

use serde::{Deserialize, Serialize};

use crate::core::extension::trace::TraceProbeConfig;
use crate::core::extension::trace::TraceSpanMetadata;
use std::collections::{BTreeMap, HashMap};

use crate::core::component::ScopedExtensionConfig;
use crate::core::extension::bench::{BenchGate, BenchGateOp};

mod check;
mod pipeline;
mod trace;
mod workload;

pub use check::{CheckSpec, NewerThanSpec, TimeSource};
pub use pipeline::{GitOp, PatchOp, PipelineStep, ServiceOp, SharedPathOp, StackOp, SymlinkOp};
pub use trace::{
    TraceDependencySpec, TraceExperimentArtifactSpec, TraceExperimentCommandSpec,
    TraceExperimentSpec, TraceGuardrailSpec, TraceNativePublicPreviewSpec,
    TracePreviewAssetFanoutSpec, TraceProfileSpec, TracePublicPreviewMode, TracePublicPreviewSpec,
    TraceVariantOverlaySpec, TraceVariantSpec,
};

/// A rig: components + services + pipelines.
///
/// Lives at `~/.config/homeboy/rigs/{id}.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RigSpec {
    /// Rig identifier. Populated from filename if empty in JSON.
    #[serde(default)]
    pub id: String,

    /// Human-readable description shown in `rig list` / `rig show`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    /// Components the rig composes (by ID). Component paths live under
    /// `ComponentSpec`, not in homeboy's `component` registry — a rig is
    /// self-contained and doesn't require components to be registered.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub components: HashMap<String, ComponentSpec>,

    /// Background services the rig manages (HTTP servers, etc.).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub services: HashMap<String, ServiceSpec>,

    /// Symlinks the rig maintains (e.g. `~/.local/bin/studio` → `studio-dev`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symlinks: Vec<SymlinkSpec>,

    /// Ephemeral dependency paths a rig may borrow from another checkout.
    ///
    /// Unlike `symlinks`, these are safe-by-default: `ensure` only creates the
    /// link when the path is missing, leaves real directories alone, and records
    /// ownership so cleanup removes only links created by this rig.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_paths: Vec<SharedPathSpec>,

    /// Shared resources this rig may exclusively own or touch while active.
    ///
    /// Phase 1 is declarative only: these are parsed, validated by serde, and
    /// displayed for operators. Runtime lock/conflict enforcement is deferred.
    #[serde(default, skip_serializing_if = "RigResourcesSpec::is_empty")]
    pub resources: RigResourcesSpec,

    /// Generic environment requirements and filesystem assertions checked by
    /// Homeboy core before rig-specific check pipelines run.
    #[serde(default, skip_serializing_if = "RigRequirementsSpec::is_empty")]
    pub requirements: RigRequirementsSpec,

    /// Pipelines for `up`, `check`, `down`, and custom verbs. MVP uses `up`,
    /// `check`, and `down`; future phases will add `sync`, `bench`, etc.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub pipeline: HashMap<String, Vec<PipelineStep>>,

    /// Bench composition settings (`homeboy rig bench`). Optional — only
    /// populated when the rig is meant to drive a benchmark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bench: Option<BenchSpec>,

    /// Fuzz composition settings (`homeboy fuzz --rig <id>`). Optional — only
    /// populated when the rig is meant to drive fuzz workloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuzz: Option<FuzzSpec>,

    /// Out-of-tree bench workloads keyed by extension id.
    ///
    /// These are private, rig-owned workloads that should run alongside the
    /// component's in-tree bench discovery when `homeboy bench --rig <id>` is
    /// invoked. Values support the same `~`, `${env.NAME}`, and
    /// `${components.<id>.path}` expansion as other rig path fields, plus
    /// `${package.root}` for rigs installed from a package source.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub bench_workloads: HashMap<String, Vec<WorkloadSpec>>,

    /// Out-of-tree trace workloads keyed by extension id.
    ///
    /// These are private, rig-owned workloads that should run alongside the
    /// component's in-tree trace discovery when `homeboy trace --rig <id>` is
    /// invoked. Values support the same `~`, `${env.NAME}`, and
    /// `${components.<id>.path}` expansion as other rig path fields, plus
    /// `${package.root}` for rigs installed from a package source.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_workloads: HashMap<String, Vec<WorkloadSpec>>,

    /// Out-of-tree fuzz workloads keyed by extension id.
    ///
    /// These are private, rig-owned workloads that should run alongside the
    /// component's in-tree fuzz discovery when `homeboy fuzz --rig <id>` is
    /// invoked. Values support the same expansion rules as bench/trace
    /// workloads, including `${package.root}` for package-installed rigs.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub fuzz_workloads: HashMap<String, Vec<WorkloadSpec>>,

    /// Extension-scoped defaults applied to every trace workload entry for the
    /// same extension id. Defaults only fill omitted scalar fields and prepend
    /// collection/map fields so per-workload declarations remain authoritative.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_workload_defaults: HashMap<String, WorkloadDefaultsSpec>,

    /// Rig-level reusable phase/span metadata templates. Workloads and workload
    /// defaults can reference these by name with `trace_phase_template`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_phase_templates: HashMap<String, TracePhaseTemplateSpec>,

    /// Named trace variants that can apply overlays across rig components.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_variants: HashMap<String, TraceVariantSpec>,

    /// Named trace profiles that resolve repeatable trace workflows to the
    /// normal trace runner contract.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_profiles: HashMap<String, TraceProfileSpec>,

    /// Named trace experiment plans that wrap a trace run with lifecycle
    /// commands, workload settings/env, and artifact collection.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_experiments: HashMap<String, TraceExperimentSpec>,

    /// Post-trace guardrails for trace experiments. These run after timing
    /// artifacts are captured so speedups cannot hide behavior regressions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_guardrails: Vec<TraceGuardrailSpec>,

    /// Named bench scenario suites keyed by profile name.
    ///
    /// `homeboy bench --rig <id> --profile <name>` resolves the profile to
    /// these scenario ids, then uses the normal scenario filtering path.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub bench_profiles: HashMap<String, Vec<String>>,

    /// Optional desktop launcher wrapper for this rig.
    ///
    /// Generates a desktop launcher that runs `homeboy rig check` and
    /// `homeboy rig up` before opening the target app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_launcher: Option<AppLauncherSpec>,
}

/// Declarative resources a rig owns or touches while active.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RigResourcesSpec {
    /// Logical resource tokens that should not overlap with another active rig.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusive: Vec<String>,

    /// Filesystem paths the rig may mutate or require exclusive access to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,

    /// TCP ports the rig may bind or assume ownership of.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,

    /// Process command-line substrings the rig may stop or inspect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process_patterns: Vec<String>,
}

impl RigResourcesSpec {
    pub fn is_empty(&self) -> bool {
        self.exclusive.is_empty()
            && self.paths.is_empty()
            && self.ports.is_empty()
            && self.process_patterns.is_empty()
    }
}

/// Declarative rig requirements checked by core without domain-specific logic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RigRequirementsSpec {
    /// Executables that must be available before the rig can be considered healthy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executables: Vec<ExecutableRequirementSpec>,

    /// Filesystem paths/files/directories the rig expects to exist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filesystem_assertions: Vec<FilesystemAssertionSpec>,

    /// Extension/provider-owned requirement declarations. Core preserves these
    /// for downstream planners without interpreting domain-specific shape.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl RigRequirementsSpec {
    pub fn is_empty(&self) -> bool {
        self.executables.is_empty()
            && self.filesystem_assertions.is_empty()
            && self.extensions.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableRequirementSpec {
    /// Executable name or path. Bare names are resolved from PATH.
    pub executable: String,

    /// Optional environment variable whose value points to the executable path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,

    /// Additional environment variables to try, in order, before PATH lookup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_aliases: Vec<String>,

    /// Human-readable label shown in `rig check` output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Human remediation shown when the requirement is not satisfied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemAssertionSpec {
    /// Path to assert. Supports `~`, `${env.NAME}`, and rig component tokens.
    pub path: String,

    /// Required path type. Defaults to any existing filesystem path.
    #[serde(default, skip_serializing_if = "FilesystemAssertionKind::is_path")]
    pub kind: FilesystemAssertionKind,

    /// Base directory for relative paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Human-readable label shown in `rig check` output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Human remediation shown when the assertion is not satisfied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Default for FilesystemAssertionSpec {
    fn default() -> Self {
        Self {
            path: String::new(),
            kind: FilesystemAssertionKind::Path,
            cwd: None,
            label: None,
            remediation: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemAssertionKind {
    #[default]
    Path,
    File,
    Dir,
}

impl FilesystemAssertionKind {
    pub fn is_path(&self) -> bool {
        matches!(self, Self::Path)
    }

    /// Human-readable label for this assertion kind.
    pub fn label(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::File => "file",
            Self::Dir => "dir",
        }
    }

    /// Whether the given path satisfies this assertion kind.
    pub fn matches_path(self, path: &std::path::Path) -> bool {
        match self {
            Self::Path => path.exists(),
            Self::File => path.is_file(),
            Self::Dir => path.is_dir(),
        }
    }
}

/// Desktop launcher settings for a rig.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLauncherSpec {
    /// Launcher platform. Supports `macos` and `linux`.
    pub platform: AppLauncherPlatform,

    /// Display name for the generated launcher.
    pub wrapper_display_name: String,

    /// Bundle identifier written to Info.plist on macOS.
    pub wrapper_bundle_id: String,

    /// Target app or executable to launch after rig prep succeeds.
    /// Supports `~`, `${env.NAME}`, and `${components.<id>.path}` expansion.
    pub target_app: String,

    /// Directory that receives the generated wrapper. Defaults to
    /// `/Applications` on macOS and `$HOME/.local/share/applications` on Linux;
    /// tests and non-global installs can point this at a writable directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_dir: Option<String>,

    /// Preflight commands to run before `rig up`. Defaults to `rig:check`.
    #[serde(
        default = "default_app_preflight",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub preflight: Vec<AppLauncherPreflight>,

    /// Failure behaviour for preflight. macOS implements the dialog + terminal
    /// script path; Linux exits the generated `.desktop` command with the
    /// failing preflight status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_preflight_fail: Option<String>,
}

/// Platform strategy for a generated desktop launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppLauncherPlatform {
    Macos,
    Linux,
}

/// Preflight command run by a generated launcher before `rig up`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppLauncherPreflight {
    #[serde(rename = "rig:check")]
    RigCheck,
}

fn default_app_preflight() -> Vec<AppLauncherPreflight> {
    vec![AppLauncherPreflight::RigCheck]
}

/// Bench composition for a rig. Pins which component(s) `homeboy bench
/// --rig <id>` benchmarks when no explicit component is passed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchSpec {
    /// Component ID to benchmark when `homeboy rig bench <rig>` is invoked
    /// without `--component`. Optional — `--component` is required at the
    /// CLI when this isn't set and `components` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_component: Option<String>,

    /// Component IDs to benchmark as one rig-pinned matrix when
    /// `homeboy bench --rig <id>` is invoked without a positional
    /// component. Each component runs independently; the command-level
    /// output merges scenarios with a `:c<component>` suffix.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,

    /// When set, `homeboy bench --rig <this-rig>` is automatically
    /// upgraded into a two-rig comparison `--rig <baseline>,<this-rig>`,
    /// with `<baseline>` resolved from this field. Closes the most
    /// common bench shape — main vs branch — into a single-flag
    /// invocation without per-call spec authoring.
    ///
    /// Ignored when:
    /// - `--rig` already lists multiple rigs (explicit beats implicit),
    /// - `--baseline` or `--ratchet` is passed (the user wants a
    ///   deliberate single-rig run that writes a baseline),
    /// - `--ignore-default-baseline` is passed (explicit opt-out).
    ///
    /// A rig that names itself as its own `default_baseline_rig` is
    /// rejected at dispatch time with a clear error — fix the spec or
    /// pass `--ignore-default-baseline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_baseline_rig: Option<String>,

    /// Warmup iterations to forward to bench runners for this rig. CLI
    /// `homeboy bench --warmup <N>` overrides this value; omitted keeps
    /// the runner's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_iterations: Option<u64>,

    /// Optional matrix axes for cross-rig bench comparison reporting.
    ///
    /// Example: `{ "runtime": "sdk", "substrate": "bfb" }`. When
    /// multiple rigs declare compatible axes, `homeboy bench --rig a,b,c,d`
    /// can emit supplemental pairwise diffs grouped by the non-varying axes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub axes: BTreeMap<String, String>,

    /// Scenario-level metric gates declared by the rig. Keys are bench
    /// scenario ids; values map metric names to pass/fail conditions.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_gates: BTreeMap<String, BTreeMap<String, BenchMetricGateCondition>>,
}

/// Fuzz composition for a rig. Pins which component `homeboy fuzz --rig <id>`
/// targets when no explicit component is passed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FuzzSpec {
    /// Component ID to fuzz when `homeboy fuzz --rig <id>` is invoked without a
    /// positional component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_component: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchMetricGateCondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<f64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gte: Option<f64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte: Option<f64>,
}

impl BenchMetricGateCondition {
    pub fn to_gates(&self, metric: &str) -> Vec<BenchGate> {
        let mut gates = Vec::new();
        if let Some(value) = self.equals {
            gates.push(BenchGate {
                metric: metric.to_string(),
                op: BenchGateOp::Eq,
                value,
            });
        }
        if let Some(value) = self.gte {
            gates.push(BenchGate {
                metric: metric.to_string(),
                op: BenchGateOp::Gte,
                value,
            });
        }
        if let Some(value) = self.lte {
            gates.push(BenchGate {
                metric: metric.to_string(),
                op: BenchGateOp::Lte,
                value,
            });
        }
        gates
    }
}

/// Shared trace phase-preset configuration.
///
/// The `{ trace_phase_presets, trace_span_metadata, trace_default_phase_preset }`
/// group is declared once here and flattened into every spec that carries it
/// (`WorkloadSpec`, `WorkloadDefaultsSpec`, `TracePhaseTemplateSpec`), so the
/// on-disk JSON keys stay flat while the field group lives in a single type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TraceConfig {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_phase_presets: HashMap<String, Vec<String>>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_span_metadata: HashMap<String, TraceSpanMetadata>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_default_phase_preset: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadSpec {
    pub path: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_phase_template: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_preview: Option<TracePublicPreviewSpec>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_groups: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_range_size: Option<u16>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_leases: Vec<String>,

    #[serde(flatten)]
    pub trace: TraceConfig,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_variants: HashMap<String, TraceVariantSpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_guardrails: Vec<TraceGuardrailSpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_probes: Vec<TraceProbeConfig>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<TraceDependencySpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkloadDefaultsSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_phase_template: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_preview: Option<TracePublicPreviewSpec>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_groups: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_range_size: Option<u16>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_leases: Vec<String>,

    #[serde(flatten)]
    pub trace: TraceConfig,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_variants: HashMap<String, TraceVariantSpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_guardrails: Vec<TraceGuardrailSpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_probes: Vec<TraceProbeConfig>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<TraceDependencySpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TracePhaseTemplateSpec {
    #[serde(flatten)]
    pub trace: TraceConfig,
}

impl WorkloadSpec {
    pub fn apply_defaults(&mut self, defaults: &WorkloadDefaultsSpec) {
        if self.trace_phase_template.is_none() {
            self.trace_phase_template = defaults.trace_phase_template.clone();
        }
        if self.public_preview.is_none() {
            self.public_preview = defaults.public_preview.clone();
        }
        if self.check_groups.is_none() {
            self.check_groups = defaults.check_groups.clone();
        }
        if self.port_range_size.is_none() {
            self.port_range_size = defaults.port_range_size;
        }
        if self.trace.trace_default_phase_preset.is_none() {
            self.trace.trace_default_phase_preset =
                defaults.trace.trace_default_phase_preset.clone();
        }

        prepend_missing(&mut self.named_leases, &defaults.named_leases);
        prepend_missing(&mut self.trace_guardrails, &defaults.trace_guardrails);
        prepend_missing(&mut self.trace_probes, &defaults.trace_probes);
        prepend_missing(&mut self.dependencies, &defaults.dependencies);
        prepend_missing(&mut self.runner_capabilities, &defaults.runner_capabilities);
        merge_defaults_map(
            &mut self.trace.trace_phase_presets,
            &defaults.trace.trace_phase_presets,
        );
        merge_defaults_map(
            &mut self.trace.trace_span_metadata,
            &defaults.trace.trace_span_metadata,
        );
        merge_defaults_map(&mut self.trace_variants, &defaults.trace_variants);
    }

    pub fn apply_phase_template(&mut self, template: &TracePhaseTemplateSpec) {
        if self.trace.trace_default_phase_preset.is_none() {
            self.trace.trace_default_phase_preset =
                template.trace.trace_default_phase_preset.clone();
        }
        merge_defaults_map(
            &mut self.trace.trace_phase_presets,
            &template.trace.trace_phase_presets,
        );
        merge_defaults_map(
            &mut self.trace.trace_span_metadata,
            &template.trace.trace_span_metadata,
        );
    }
}

fn prepend_missing<T: Clone + PartialEq>(target: &mut Vec<T>, defaults: &[T]) {
    if defaults.is_empty() {
        return;
    }
    let mut merged = defaults.to_vec();
    for item in target.iter() {
        if !merged.contains(item) {
            merged.push(item.clone());
        }
    }
    *target = merged;
}

fn merge_defaults_map<T: Clone>(target: &mut HashMap<String, T>, defaults: &HashMap<String, T>) {
    for (key, value) in defaults {
        target.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_gates() {
        let condition = BenchMetricGateCondition {
            equals: Some(1.0),
            gte: Some(0.5),
            lte: Some(2.0),
        };

        let gates = condition.to_gates("native_block_quality_pass");

        assert_eq!(gates.len(), 3);
        assert!(gates.iter().any(|gate| {
            gate.metric == "native_block_quality_pass"
                && gate.op == BenchGateOp::Eq
                && gate.value == 1.0
        }));
        assert!(gates.iter().any(|gate| {
            gate.metric == "native_block_quality_pass"
                && gate.op == BenchGateOp::Gte
                && gate.value == 0.5
        }));
        assert!(gates.iter().any(|gate| {
            gate.metric == "native_block_quality_pass"
                && gate.op == BenchGateOp::Lte
                && gate.value == 2.0
        }));
        assert!(BenchMetricGateCondition {
            equals: None,
            gte: None,
            lte: None,
        }
        .to_gates("metric")
        .is_empty());
    }

    #[test]
    fn test_trace_phase_preset() {
        let workload = WorkloadSpec {
            path: "trace.mjs".to_string(),
            trace_phase_template: None,
            public_preview: None,
            check_groups: None,
            port_range_size: None,
            named_leases: Vec::new(),
            trace: TraceConfig {
                trace_phase_presets: HashMap::from([(
                    "startup".to_string(),
                    vec!["launch".to_string(), "ready".to_string()],
                )]),
                trace_span_metadata: HashMap::new(),
                trace_default_phase_preset: None,
            },
            trace_variants: HashMap::new(),
            trace_guardrails: Vec::new(),
            trace_probes: Vec::new(),
            dependencies: Vec::new(),
            runner_capabilities: Vec::new(),
        };

        assert_eq!(workload.trace_phase_preset("missing"), None);
        assert_eq!(
            workload.trace_phase_preset("startup"),
            Some(["launch".to_string(), "ready".to_string()].as_slice())
        );
        let workload_without_preset: WorkloadSpec =
            serde_json::from_str(r#"{"path":"trace.mjs"}"#).expect("parse workload");
        assert_eq!(workload_without_preset.trace_phase_preset("startup"), None);
    }

    #[test]
    fn test_trace_span_metadata() {
        let workload: WorkloadSpec = serde_json::from_str(
            r#"{
                "path": "/tmp/scoped.trace.mjs",
                "trace_span_metadata": {
                    "phase.boot_to_ready": {
                        "critical": true,
                        "blocking": true,
                        "cacheable": true,
                        "prewarmable": true,
                        "blocks": "first_site_render",
                        "category": "wordpress_boot"
                    }
                }
            }"#,
        )
        .expect("parse detailed workload metadata");

        let metadata = workload
            .trace_span_metadata()
            .get("phase.boot_to_ready")
            .expect("span metadata");
        assert!(metadata.critical);
        assert!(metadata.blocking);
        assert!(metadata.cacheable);
        assert!(metadata.prewarmable);
        assert_eq!(metadata.blocks.as_deref(), Some("first_site_render"));
        assert_eq!(metadata.category.as_deref(), Some("wordpress_boot"));
        let workload_without_metadata: WorkloadSpec =
            serde_json::from_str(r#"{"path":"/tmp/no-metadata.trace.mjs"}"#)
                .expect("parse workload");
        assert!(workload_without_metadata.trace_span_metadata().is_empty());
    }

    #[test]
    fn test_trace_probes() {
        let workload: WorkloadSpec = serde_json::from_str(
            r#"{
                "path": "/tmp/scoped.trace.mjs",
                "trace_probes": [
                    { "type": "log.tail", "path": "/tmp/app.log", "grep": "ready" },
                    { "type": "process.snapshot", "pattern": "node.*serve", "interval_ms": 250 },
                    { "type": "file.watch", "path": "/tmp/auth.json", "interval_ms": 100 },
                    { "type": "port.snapshot", "port": 3000 },
                    { "type": "http.poll", "url": "http://127.0.0.1:3000/health", "assert-status": 200 },
                    { "type": "http.egress", "host": "api.example.com", "capture": "headers" },
                    { "type": "cmd.run", "command": "kimaki", "args": ["--help"] }
                ]
            }"#,
        )
        .expect("parse detailed workload probes");

        assert_eq!(workload.trace_probes().len(), 7);
        assert!(matches!(
            &workload.trace_probes()[0],
            TraceProbeConfig::LogTail { path, grep, .. }
                if path == "/tmp/app.log" && grep.as_deref() == Some("ready")
        ));
        assert!(matches!(
            &workload.trace_probes()[1],
            TraceProbeConfig::ProcessSnapshot { pattern, interval_ms }
                if pattern == "node.*serve" && *interval_ms == Some(250)
        ));
        assert!(matches!(
            &workload.trace_probes()[2],
            TraceProbeConfig::FileWatch { path, interval_ms }
                if path == "/tmp/auth.json" && *interval_ms == Some(100)
        ));
        assert!(matches!(
            &workload.trace_probes()[3],
            TraceProbeConfig::PortSnapshot { port, .. }
                if *port == Some(3000)
        ));
        assert!(matches!(
            &workload.trace_probes()[4],
            TraceProbeConfig::HttpPoll { url, assert_status, .. }
                if url == "http://127.0.0.1:3000/health" && *assert_status == Some(200)
        ));
        assert!(matches!(
            &workload.trace_probes()[5],
            TraceProbeConfig::HttpEgress { host, capture, .. }
                if host == "api.example.com" && capture == "headers"
        ));
        assert!(matches!(
            &workload.trace_probes()[6],
            TraceProbeConfig::CmdRun { command, args }
                if command == "kimaki" && args == &vec!["--help".to_string()]
        ));
        let workload_without_probes: WorkloadSpec =
            serde_json::from_str(r#"{"path":"/tmp/no-probes.trace.mjs"}"#).expect("parse workload");
        assert!(workload_without_probes.trace_probes().is_empty());
    }

    #[test]
    fn test_string_settings() {
        let profile: TraceProfileSpec = serde_json::from_str(
            r#"{
                "settings": {
                    "title": "Studio",
                    "retry_count": 2
                }
            }"#,
        )
        .expect("parse profile");

        assert_eq!(
            profile.string_settings(),
            vec![("title".to_string(), "Studio".to_string())]
        );
    }

    #[test]
    fn test_json_settings() {
        let profile: TraceProfileSpec = serde_json::from_str(
            r#"{
                "settings": {
                    "title": "Studio",
                    "retry_count": 2,
                    "options": { "headless": true }
                }
            }"#,
        )
        .expect("parse profile");

        assert_eq!(
            profile.json_settings(),
            vec![
                (
                    "options".to_string(),
                    serde_json::json!({ "headless": true })
                ),
                ("retry_count".to_string(), serde_json::json!(2))
            ]
        );
    }

    #[test]
    fn test_trace_default_phase_preset() {
        let workload = WorkloadSpec {
            path: "trace.mjs".to_string(),
            trace_phase_template: None,
            public_preview: None,
            check_groups: None,
            port_range_size: None,
            named_leases: Vec::new(),
            trace: TraceConfig {
                trace_phase_presets: HashMap::new(),
                trace_span_metadata: HashMap::new(),
                trace_default_phase_preset: Some("startup".to_string()),
            },
            trace_variants: HashMap::new(),
            trace_guardrails: Vec::new(),
            trace_probes: Vec::new(),
            dependencies: Vec::new(),
            runner_capabilities: Vec::new(),
        };

        assert_eq!(workload.trace_default_phase_preset(), Some("startup"));
        let workload_without_default: WorkloadSpec =
            serde_json::from_str(r#"{"path":"trace.mjs"}"#).expect("parse workload");
        assert_eq!(workload_without_default.trace_default_phase_preset(), None);
    }

    #[test]
    fn test_port_range_size() {
        let workload = WorkloadSpec {
            path: "bench.mjs".to_string(),
            trace_phase_template: None,
            public_preview: None,
            check_groups: None,
            port_range_size: Some(8),
            named_leases: Vec::new(),
            trace: TraceConfig {
                trace_phase_presets: HashMap::new(),
                trace_span_metadata: HashMap::new(),
                trace_default_phase_preset: None,
            },
            trace_variants: HashMap::new(),
            trace_guardrails: Vec::new(),
            trace_probes: Vec::new(),
            dependencies: Vec::new(),
            runner_capabilities: Vec::new(),
        };

        assert_eq!(workload.port_range_size(), Some(8));
        let workload_without_ports: WorkloadSpec =
            serde_json::from_str(r#"{"path":"bench.mjs"}"#).expect("parse workload");
        assert_eq!(workload_without_ports.port_range_size(), None);
    }

    #[test]
    fn test_named_leases() {
        let workload = WorkloadSpec {
            path: "bench.mjs".to_string(),
            trace_phase_template: None,
            public_preview: None,
            check_groups: None,
            port_range_size: None,
            named_leases: vec!["browser-profile".to_string()],
            trace: TraceConfig {
                trace_phase_presets: HashMap::new(),
                trace_span_metadata: HashMap::new(),
                trace_default_phase_preset: None,
            },
            trace_variants: HashMap::new(),
            trace_guardrails: Vec::new(),
            trace_probes: Vec::new(),
            dependencies: Vec::new(),
            runner_capabilities: Vec::new(),
        };

        assert_eq!(workload.named_leases(), &["browser-profile".to_string()]);
        let workload_without_leases: WorkloadSpec =
            serde_json::from_str(r#"{"path":"bench.mjs"}"#).expect("parse workload");
        assert!(workload_without_leases.named_leases().is_empty());
    }

    #[test]
    fn test_trace_guardrails_parse_at_rig_workload_and_variant_scope() {
        let spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "studio-rig",
                "trace_guardrails": [
                    { "label": "health", "http": "http://127.0.0.1:3000/health" }
                ],
                "trace_workloads": {
                    "fixture-trace": [
                        {
                            "path": "trace/create-site.trace.mjs",
                            "trace_guardrails": [
                                { "label": "list sites", "command": "npm run smoke:list-sites" }
                            ],
                            "trace_variants": {
                                "fast-install": {
                                    "overlay": "overlays/fast-install.patch",
                                    "trace_guardrails": [
                                        { "label": "install smoke", "command": "npm run smoke:install" }
                                    ]
                                }
                            }
                        }
                    ]
                }
            }"#,
        )
        .expect("parse guardrails");

        assert_eq!(spec.trace_guardrails[0].label.as_deref(), Some("health"));
        assert_eq!(
            spec.trace_guardrails[0].check.http.as_deref(),
            Some("http://127.0.0.1:3000/health")
        );
        let workload = spec.trace_workloads["fixture-trace"]
            .first()
            .expect("workload");
        assert_eq!(
            workload.trace_guardrails()[0].check.command.as_deref(),
            Some("npm run smoke:list-sites")
        );
        let variants = workload.trace_variants();
        assert_eq!(
            variants["fast-install"].trace_guardrails[0]
                .check
                .command
                .as_deref(),
            Some("npm run smoke:install")
        );
    }
}

/// Component reference inside a rig spec. Decoupled from the global component
/// registry because rigs should work even when a component isn't registered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSpec {
    /// Local filesystem path to the component checkout. Supports `~` and
    /// `${env.VAR}` expansion at use time.
    pub path: String,

    /// Optional checkout root used when `path` points at a subdirectory inside
    /// the repository. Lab runner materialization uses this root while rig
    /// expansion keeps `path` as the component's effective path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_root: Option<String>,

    /// Optional source repository URL. When omitted, `homeboy triage rig`
    /// falls back to `git -C <path> remote get-url origin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,

    /// Reporting-only GitHub remote override for `homeboy triage rig`.
    /// Does not affect git, deploy, release, or rig pipeline operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage_remote_url: Option<String>,

    /// Stack ID this component should track. `homeboy rig sync` and explicit
    /// `stack` pipeline steps use this to delegate combined-fixes upkeep to
    /// the stack primitive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,

    /// Optional branch hint for `rig status`. MVP just reports actual branch;
    /// this field documents expected branch for humans reading specs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Explicit pinned ref for Lab dependency materialization. When set,
    /// Homeboy records the checkout as intentionally pinned instead of trying
    /// to prove latest-branch freshness for detached or non-upstream checkouts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,

    /// Optional extension config for rig-owned bench dispatch.
    ///
    /// This is intentionally narrower than the global component registry: rigs
    /// may provide the extension settings needed by `homeboy bench --rig`, but
    /// release/deploy/component-management semantics still belong to registered
    /// components or repo-owned `homeboy.json` files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<HashMap<String, ScopedExtensionConfig>>,
}

/// A background service the rig manages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// Service kind — drives which strategy `service::start` uses.
    pub kind: ServiceKind,

    /// Working directory for the service process. Supports `~` and
    /// `${components.X.path}` / `${env.VAR}` variable expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// TCP port the service binds to. Used by `http-static` to construct the
    /// python command, and surfaced in `rig status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,

    /// Arbitrary shell command (only used by `kind = "command"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Environment variables passed to the service process.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Health check evaluated by `rig check`. Optional; if absent, a service
    /// is healthy if its PID is alive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<CheckSpec>,

    /// Adoption strategy for `kind = "external"` — how to find a process
    /// the rig didn't spawn so `service.stop` can signal it. Required for
    /// `external`, ignored for other kinds. The narrow shape here is
    /// intentional MVP: only one discovery method (`pgrep`-style pattern
    /// match) and only the `stop` op honors it. Full local supervision
    /// of adopted services is tracked in Extra-Chill/homeboy#1463.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discover: Option<DiscoverSpec>,
}

/// Discovery strategy for an `external` service — how to find a PID the rig
/// didn't spawn. The `pattern` field preserves the original broad substring
/// match against the process command line (`ps -o args`); optional selectors
/// narrow that candidate set. `kind = "external"` services pick the newest
/// matching PID. Multiple matches are not an error — a stale child + a fresh
/// child is the case we care about, and the fresh one is what the rig wants to
/// interact with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverSpec {
    /// Substring that must appear in the target process's command line.
    /// Matched against `ps -o args= -p <pid>` output, so users can pin
    /// against script paths (`wordpress-server-child.mjs`) or argv tokens.
    pub pattern: String,

    /// Additional argv substrings that must all appear in the target process's
    /// command line. Use this to keep a broad `pattern` fallback while pinning
    /// an external service to a more specific script path or flag set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv_contains: Vec<String>,
}

/// Supported service kinds. Extensions will register more in a future phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceKind {
    /// `python3 -m http.server <port>` in `cwd`. Common enough to be built in.
    HttpStatic,
    /// Arbitrary shell command. Everything else.
    Command,
    /// Process the rig didn't spawn — discovered via `discover.pattern`.
    /// Only `stop` is meaningful (signals the discovered PID); `start`
    /// returns a clear error because rig isn't responsible for launching
    /// adopted services. Use case: stale daemons that the rig needs to
    /// recycle after a build (e.g. Studio's `wordpress-server-child.mjs`
    /// after a Studio CLI rebuild).
    External,
}

/// Symlink the rig maintains. Both paths support `~` expansion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymlinkSpec {
    /// Link path (the symlink itself).
    pub link: String,
    /// Target path the link points to.
    pub target: String,
}

/// Ephemeral path borrowed from another checkout, usually dependencies such as
/// `node_modules` that can be reused across worktrees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedPathSpec {
    /// Path inside the active checkout. If missing, `shared-path ensure` creates
    /// a symlink here. If a real file/directory already exists, it is left alone.
    pub link: String,
    /// Existing path to borrow, usually the primary checkout's dependency dir.
    pub target: String,
}

#[cfg(test)]
mod trace_experiment_spec_tests {
    use super::{RigSpec, TraceExperimentArtifactSpec};

    #[test]
    fn trace_experiments_parse_lifecycle_settings_and_artifacts() {
        let json = r#"{
            "id": "studio-playground-dev",
            "trace_experiments": {
                "template-site": {
                    "setup": [
                        { "command": "node bench/create-template-site.mjs", "cwd": "${package.root}" }
                    ],
                    "settings": {
                        "STUDIO_TRACE_SITE_TEMPLATE": "/tmp/studio-template-site",
                        "USE_TEMPLATE": true
                    },
                    "env": {
                        "STUDIO_EXPERIMENT_MODE": "template"
                    },
                    "artifacts": [
                        "/tmp/studio-template-site/report.json",
                        { "label": "template log", "path": "/tmp/studio-template-site/template.log" }
                    ],
                    "teardown": [
                        { "command": "rm -rf /tmp/studio-template-site" }
                    ]
                }
            }
        }"#;
        let spec: RigSpec = serde_json::from_str(json).expect("parse");
        let experiment = spec
            .trace_experiments
            .get("template-site")
            .expect("experiment");

        assert_eq!(
            experiment.setup[0].command,
            "node bench/create-template-site.mjs"
        );
        assert_eq!(experiment.setup[0].cwd.as_deref(), Some("${package.root}"));
        assert_eq!(
            experiment.settings["STUDIO_TRACE_SITE_TEMPLATE"],
            serde_json::Value::String("/tmp/studio-template-site".to_string())
        );
        assert_eq!(
            experiment.settings["USE_TEMPLATE"],
            serde_json::Value::Bool(true)
        );
        assert_eq!(
            experiment
                .env
                .get("STUDIO_EXPERIMENT_MODE")
                .map(String::as_str),
            Some("template")
        );
        assert!(matches!(
            &experiment.artifacts[0],
            TraceExperimentArtifactSpec::Path(path) if path == "/tmp/studio-template-site/report.json"
        ));
        assert!(matches!(
            &experiment.artifacts[1],
            TraceExperimentArtifactSpec::Detailed { label, path }
                if label == "template log" && path == "/tmp/studio-template-site/template.log"
        ));
        assert_eq!(
            experiment.teardown[0].command,
            "rm -rf /tmp/studio-template-site"
        );
    }
}

#[cfg(test)]
#[path = "../../../tests/core/rig/spec_test.rs"]
mod spec_test;

#[cfg(test)]
#[path = "../../../tests/core/rig/public_preview_spec_test.rs"]
mod public_preview_spec_test;

#[cfg(test)]
#[path = "../../../tests/core/rig/bench_default_baseline_spec_test.rs"]
mod bench_default_baseline_spec_test;
