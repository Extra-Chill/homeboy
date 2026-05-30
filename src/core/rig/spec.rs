//! Rig spec types — the JSON schema on disk.

use serde::{Deserialize, Serialize};

use crate::core::extension::trace::TraceProbeConfig;
use crate::core::extension::trace::TraceSpanMetadata;
use std::collections::{BTreeMap, HashMap};

use crate::core::component::ScopedExtensionConfig;
use crate::core::extension::bench::{BenchGate, BenchGateOp};

/// A rig: components + services + pipelines.
///
/// Lives at `~/.config/homeboy/rigs/{id}.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Pipelines for `up`, `check`, `down`, and custom verbs. MVP uses `up`,
    /// `check`, and `down`; future phases will add `sync`, `bench`, etc.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub pipeline: HashMap<String, Vec<PipelineStep>>,

    /// Bench composition settings (`homeboy rig bench`). Optional — only
    /// populated when the rig is meant to drive a benchmark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bench: Option<BenchSpec>,

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

/// Rig-owned extension workload declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadSpec {
    pub path: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_groups: Option<Vec<String>>,

    /// Number of contiguous local ports this workload needs for one child
    /// invocation. Homeboy allocates non-overlapping ranges and exposes them as
    /// HOMEBOY_INVOCATION_PORT_BASE/MAX.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_range_size: Option<u16>,

    /// Logical machine-local resources this workload requires exclusively while
    /// its child process is running.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_leases: Vec<String>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_phase_presets: HashMap<String, Vec<String>>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_span_metadata: HashMap<String, TraceSpanMetadata>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_default_phase_preset: Option<String>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_variants: HashMap<String, TraceVariantSpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_guardrails: Vec<TraceGuardrailSpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_probes: Vec<TraceProbeConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceVariantSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TraceVariantOverlaySpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_guardrails: Vec<TraceGuardrailSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceProfileSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, serde_json::Value>,
}

impl TraceProfileSpec {
    pub fn string_settings(&self) -> Vec<(String, String)> {
        self.settings
            .iter()
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(|string| (key.clone(), string.to_string()))
            })
            .collect()
    }

    pub fn json_settings(&self) -> Vec<(String, serde_json::Value)> {
        self.settings
            .iter()
            .filter(|(_, value)| !value.is_string())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceGuardrailSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    #[serde(flatten)]
    pub check: CheckSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceVariantOverlaySpec {
    pub component: String,
    pub overlay: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceExperimentSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<TraceExperimentCommandSpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown: Vec<TraceExperimentCommandSpec>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, serde_json::Value>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<TraceExperimentArtifactSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceExperimentCommandSpec {
    pub command: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TraceExperimentArtifactSpec {
    Path(String),
    Detailed { label: String, path: String },
}

impl WorkloadSpec {
    pub fn path(&self) -> &str {
        &self.path
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
        self.trace_phase_presets
            .get(name)
            .map(|phases| phases.as_slice())
    }

    pub fn trace_span_metadata(&self) -> &HashMap<String, TraceSpanMetadata> {
        &self.trace_span_metadata
    }

    pub fn trace_default_phase_preset(&self) -> Option<&str> {
        self.trace_default_phase_preset.as_deref()
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
            check_groups: None,
            port_range_size: None,
            named_leases: Vec::new(),
            trace_phase_presets: HashMap::from([(
                "startup".to_string(),
                vec!["launch".to_string(), "ready".to_string()],
            )]),
            trace_span_metadata: HashMap::new(),
            trace_default_phase_preset: None,
            trace_variants: HashMap::new(),
            trace_guardrails: Vec::new(),
            trace_probes: Vec::new(),
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
            check_groups: None,
            port_range_size: None,
            named_leases: Vec::new(),
            trace_phase_presets: HashMap::new(),
            trace_span_metadata: HashMap::new(),
            trace_default_phase_preset: Some("startup".to_string()),
            trace_variants: HashMap::new(),
            trace_guardrails: Vec::new(),
            trace_probes: Vec::new(),
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
            check_groups: None,
            port_range_size: Some(8),
            named_leases: Vec::new(),
            trace_phase_presets: HashMap::new(),
            trace_span_metadata: HashMap::new(),
            trace_default_phase_preset: None,
            trace_variants: HashMap::new(),
            trace_guardrails: Vec::new(),
            trace_probes: Vec::new(),
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
            check_groups: None,
            port_range_size: None,
            named_leases: vec!["browser-profile".to_string()],
            trace_phase_presets: HashMap::new(),
            trace_span_metadata: HashMap::new(),
            trace_default_phase_preset: None,
            trace_variants: HashMap::new(),
            trace_guardrails: Vec::new(),
            trace_probes: Vec::new(),
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
                    "nodejs": [
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
        let workload = spec.trace_workloads["nodejs"].first().expect("workload");
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

/// A pipeline step. Flat enum via `kind` discriminator so specs stay readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PipelineStep {
    /// Start/stop/health-check a declared service.
    Service {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Service ID (must exist in `services`).
        id: String,
        /// Operation: `start`, `stop`, or `health`.
        op: ServiceOp,
    },

    /// Delegate to `homeboy build`.
    ///
    /// Rigs should prefer `build` over `command` for component builds so they
    /// pick up the component's declared `scripts.build`, extension hooks, and
    /// error-formatting surface instead of shelling out blindly. Component
    /// path is resolved from the rig's `components` map, so the component
    /// doesn't need to be registered in homeboy's global registry.
    Build {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Component ID — must exist in the rig's `components` map.
        component: String,
        /// Human-readable label shown during execution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Delegate a component lifecycle operation to its configured extension.
    ///
    /// V1 intentionally exposes only operations that Homeboy core already knows
    /// how to dispatch through extension infrastructure. Use `command` for
    /// one-off shell escape hatches; add new extension ops only when the
    /// extension layer owns the corresponding lifecycle contract.
    Extension {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Component ID — must exist in the rig's `components` map.
        component: String,
        /// Extension-owned operation. V1 supports `build`.
        op: String,
        /// Human-readable label shown during execution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Delegate to `homeboy git`.
    ///
    /// Wraps homeboy's own git primitive with a path override so rigs can
    /// operate on unregistered checkouts. Supports the subset of operations
    /// rigs actually need (MVP): `status`, `pull`, `fetch`, `checkout`,
    /// `current-branch`. More can land as follow-up.
    Git {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Component ID — must exist in the rig's `components` map.
        component: String,
        /// Operation name.
        op: GitOp,
        /// Extra git arguments, appended after the op-specific base args
        /// (e.g. `pull` with `["origin", "trunk"]` runs `git pull origin trunk`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Human-readable label shown during execution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Delegate to a declared component's stack spec.
    ///
    /// This is intentionally explicit: rigs only rewrite combined-fixes
    /// branches when a pipeline author opts into a `stack` step (or the user
    /// runs `homeboy rig sync`). `rig up` never syncs stacks implicitly.
    Stack {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Component ID — must exist in the rig's `components` map and declare
        /// a `stack` field.
        component: String,
        /// Stack operation.
        op: StackOp,
        /// Print what WOULD happen without mutating the stack spec or target
        /// branch. Only meaningful for `op = "sync"` today.
        #[serde(default)]
        dry_run: bool,
        /// Human-readable label shown during execution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Run an arbitrary shell command — escape hatch for operations that
    /// don't map to a homeboy primitive (waits, custom tooling, probes).
    ///
    /// Prefer `build` / `git` / `check` over `command` wherever they fit:
    /// typed steps pick up homeboy's existing error mapping, extension
    /// hooks, and registry awareness for free.
    Command {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Shell command to execute. Runs via `sh -c` (or `cmd /C` on Windows).
        #[serde(rename = "command")]
        cmd: String,
        /// Working directory. Supports variable expansion.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Env vars (merged over inherited environment).
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
        /// Human-readable label shown during execution. If absent, `cmd` is used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Run a shell command only when a filesystem path is missing.
    ///
    /// This keeps common bootstrap guards declarative without teaching Homeboy
    /// package-manager-specific install semantics.
    CommandIfMissing {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Path whose absence triggers the command. Relative paths resolve
        /// against `cwd` when set, otherwise against the process cwd.
        missing: String,
        /// Shell command to execute when `missing` does not exist.
        #[serde(rename = "command")]
        cmd: String,
        /// Working directory. Supports variable expansion.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Env vars (merged over inherited environment).
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
        /// Human-readable label shown during execution. If absent, `cmd` is used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Ensure a declared symlink exists (or verify it in `check` pipelines).
    Symlink {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Operation: `ensure` or `verify`.
        op: SymlinkOp,
    },

    /// Ensure, verify, or clean up declared shared dependency paths.
    SharedPath {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Operation: `ensure`, `verify`, or `cleanup`.
        op: SharedPathOp,
    },

    /// Apply (or verify) an idempotent local-only patch to a file in a
    /// component checkout. Use case: upstream-source patches that can't be
    /// committed to the consumer branch because rebases would carry them
    /// to every fresh checkout. Examples: TSRMLS_CC fallback macros on
    /// upstream Playground C sources, build-time tweaks that aren't
    /// upstream yet.
    ///
    /// Idempotency is marker-based: if `marker` is already present in
    /// the file, the step is a no-op. If the optional `after` anchor is
    /// set and absent from the file, the step errors instead of guessing
    /// where to insert (file structure changed → fail loudly).
    ///
    /// In `verify` mode the step only confirms the marker is present
    /// without modifying — mirrors how a `check` pipeline would `grep`
    /// for it. Use this in `check` pipelines so a stale or unpatched
    /// checkout is reported as a failure, not silently re-patched.
    Patch {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Component ID — must exist in the rig's `components` map.
        component: String,
        /// File to patch, relative to the component's path. Tilde +
        /// `${components.X.path}` / `${env.VAR}` expansion applies.
        file: String,
        /// Substring that uniquely identifies this patch in the file.
        /// Used as the idempotency key — present means "already patched."
        marker: String,
        /// Optional anchor: substring that must already be in the file
        /// for the patch to apply. The patch content is inserted on the
        /// next line after the first occurrence. If absent and `after`
        /// is set, the step fails (file structure changed). When `after`
        /// is `None`, the patch is appended to the end of the file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
        /// Patch content to insert. Must contain `marker` somewhere so
        /// future runs detect it as already-applied — the step validates
        /// this at apply time.
        content: String,
        /// Operation: `apply` (mutate file) or `verify` (read-only check).
        #[serde(default = "default_patch_op")]
        op: PatchOp,
        /// Human-readable label shown during execution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Pre-flight / health check. Non-fatal in `up` (warns), fatal in `check`.
    Check {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Named preflight groups this check belongs to. Workload commands can
        /// run only the groups required by a rig-owned workload while full
        /// `rig check` continues to evaluate every check-pipeline step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        groups: Vec<String>,
        /// Human-readable label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// The actual check spec.
        #[serde(flatten)]
        spec: CheckSpec,
    },
}

fn default_patch_op() -> PatchOp {
    PatchOp::Apply
}

/// Git operation supported by a rig `git` step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitOp {
    /// `git status --porcelain=v1`. Passes if exit 0.
    Status,
    /// `git pull [args...]`.
    Pull,
    /// `git push [args...]`. Use `args` for `--force-with-lease`,
    /// `--follow-tags`, etc. Plain `--force` is intentionally NOT
    /// blocked at the rig layer — rigs can be reproduced or reverted, so
    /// the safety boundary lives at the CLI surface.
    Push,
    /// `git fetch [args...]`.
    Fetch,
    /// `git checkout [args...]`.
    Checkout,
    /// `git rev-parse --abbrev-ref HEAD` — returns current branch in logs.
    CurrentBranch,
    /// `git rebase [<onto>]`. Default with no `args` rebases onto
    /// `@{upstream}`. Use `args` to specify the upstream ref or extra
    /// rebase flags.
    Rebase,
    /// `git cherry-pick <refs...>`. `args` is the list of commit refs to
    /// pick (SHAs, branches, ranges). PR-number expansion via `gh` is a
    /// CLI-only convenience and not modelled at the rig step level —
    /// resolve PR numbers to SHAs in the rig spec.
    CherryPick,
}

/// Stack operation supported by a rig `stack` step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StackOp {
    /// Delegate to `homeboy stack sync <stack-id>`.
    Sync,
}

/// Service operation in a pipeline step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceOp {
    Start,
    Stop,
    Health,
}

/// Symlink operation in a pipeline step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymlinkOp {
    Ensure,
    Verify,
}

/// Shared path operation in a pipeline step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SharedPathOp {
    /// Create missing dependency paths as symlinks to their shared targets.
    Ensure,
    /// Check that each dependency path is available without mutating anything.
    Verify,
    /// Remove only symlinks this rig created and still owns.
    Cleanup,
}

/// Patch operation in a pipeline step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PatchOp {
    /// Apply the patch if its marker is absent. No-op if already applied.
    Apply,
    /// Read-only: pass if the marker is present, fail otherwise. Use in
    /// `check` pipelines to surface stale or unpatched checkouts.
    Verify,
}

/// A single declarative check. One-of semantics — exactly one of the
/// probe fields (`http`, `file`, `any_file_exists`, `command`, `newer_than`)
/// should be set.
/// Validated at check-time, not parse-time, because serde flattening
/// across tagged enums is awkward and explicit-field checks keep the
/// spec readable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSpec {
    /// HTTP GET — passes if status matches `expect_status` (default 200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<String>,

    /// Expected HTTP status for the `http` check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_status: Option<u16>,

    /// File path — passes if the file exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// If set along with `file`, also requires the file contents to contain
    /// this substring. Cheap probe for verifying drop-ins / generated files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,

    /// File paths — passes when at least one path exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any_file_exists: Vec<String>,

    /// Shell command — passes if exit code matches `expect_exit` (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Expected exit code for the `command` check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_exit: Option<i32>,

    /// Mtime / staleness comparison — passes when `left` is newer than
    /// `right`. Surfaces "I rebuilt but the daemon is still on the old
    /// bundle" failures the wiki preflight calls out as the #1 dev-env
    /// confusion source. If the `process_start` source resolves to no
    /// running process, the check passes (no stale daemon to recycle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer_than: Option<NewerThanSpec>,
}

/// Mtime / staleness comparison check.
///
/// Each side picks one source. `left > right` ⇒ pass. Equal or `left < right`
/// ⇒ fail. "Source missing" semantics differ by side: if `left` is a
/// `process_start` and no process matches, the check passes (interpretation:
/// no stale daemon to fight with). Any other missing source is an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewerThanSpec {
    pub left: TimeSource,
    pub right: TimeSource,
}

/// A time source for `newer_than` checks. One-of semantics enforced at
/// evaluate-time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSource {
    /// File mtime (seconds since epoch). Path supports `~` and `${...}`
    /// expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_mtime: Option<String>,

    /// Process start time (seconds since epoch). Discovers the newest
    /// matching process by command-line substring (`ps -o args`). When no
    /// process matches and this source is on the `left`, the parent check
    /// passes — there's no stale process to flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start: Option<DiscoverSpec>,
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
#[path = "../../../tests/core/rig/bench_default_baseline_spec_test.rs"]
mod bench_default_baseline_spec_test;
