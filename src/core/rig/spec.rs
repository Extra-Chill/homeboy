//! Rig spec types — the JSON schema on disk.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

    /// Pipelines for `up`, `check`, `down`, and custom verbs. MVP uses `up`,
    /// `check`, and `down`; future phases will add `sync`, `bench`, etc.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub pipeline: HashMap<String, Vec<PipelineStep>>,

    /// Bench composition settings (`homeboy rig bench`). Optional — only
    /// populated when the rig is meant to drive a benchmark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bench: Option<BenchSpec>,
}

/// Bench composition for a rig. Currently minimal — pins which component
/// `homeboy rig bench` benchmarks when no explicit `--component` flag is
/// passed. Matrix expansion + custom precheck + per-component overrides
/// are deferred to follow-ups (see Extra-Chill/homeboy#1466 follow-up
/// issues).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchSpec {
    /// Component ID to benchmark when `homeboy rig bench <rig>` is invoked
    /// without `--component`. Optional — `--component` is required at the
    /// CLI when this isn't set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_component: Option<String>,
}

/// Component reference inside a rig spec. Decoupled from the global component
/// registry because rigs should work even when a component isn't registered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSpec {
    /// Local filesystem path to the component checkout. Supports `~` and
    /// `${env.VAR}` expansion at use time.
    pub path: String,

    /// Stack ID this component should track (Phase 2 — not enforced in MVP,
    /// but the field is reserved so existing specs don't break on upgrade).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,

    /// Optional branch hint for `rig status`. MVP just reports actual branch;
    /// this field documents expected branch for humans reading specs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
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
}

/// Supported service kinds. Extensions will register more in a future phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceKind {
    /// `python3 -m http.server <port>` in `cwd`. Common enough to be built in.
    HttpStatic,
    /// Arbitrary shell command. Everything else.
    Command,
}

/// Symlink the rig maintains. Both paths support `~` expansion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymlinkSpec {
    /// Link path (the symlink itself).
    pub link: String,
    /// Target path the link points to.
    pub target: String,
}

/// A pipeline step. Flat enum via `kind` discriminator so specs stay readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PipelineStep {
    /// Start/stop/health-check a declared service.
    Service {
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
        /// Component ID — must exist in the rig's `components` map.
        component: String,
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

    /// Run an arbitrary shell command — escape hatch for operations that
    /// don't map to a homeboy primitive (waits, custom tooling, probes).
    ///
    /// Prefer `build` / `git` / `check` over `command` wherever they fit:
    /// typed steps pick up homeboy's existing error mapping, extension
    /// hooks, and registry awareness for free.
    Command {
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

    /// Ensure a declared symlink exists (or verify it in `check` pipelines).
    Symlink {
        /// Operation: `ensure` or `verify`.
        op: SymlinkOp,
    },

    /// Pre-flight / health check. Non-fatal in `up` (warns), fatal in `check`.
    Check {
        /// Human-readable label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// The actual check spec.
        #[serde(flatten)]
        spec: CheckSpec,
    },
}

/// Git operation supported by a rig `git` step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitOp {
    /// `git status --porcelain=v1`. Passes if exit 0.
    Status,
    /// `git pull [args...]`.
    Pull,
    /// `git fetch [args...]`.
    Fetch,
    /// `git checkout [args...]`.
    Checkout,
    /// `git rev-parse --abbrev-ref HEAD` — returns current branch in logs.
    CurrentBranch,
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

/// A single declarative check. One-of semantics — exactly one of the fields
/// below should be set. Validated at check-time, not parse-time, because
/// serde flattening across tagged enums is awkward and explicit-field checks
/// keep the spec readable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

    /// Shell command — passes if exit code matches `expect_exit` (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Expected exit code for the `command` check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_exit: Option<i32>,
}

#[cfg(test)]
#[path = "../../../tests/core/rig/spec_test.rs"]
mod spec_test;
