//! Pipeline step schema for rig specs — the `pipeline` map's step variants
//! plus the operation enums each step references.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::CheckSpec;

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
        /// Generic capability IDs required before this dependency step runs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requires_capabilities: Vec<String>,
        /// Generic provider IDs required before this dependency step runs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requires_providers: Vec<String>,
        /// Generic capability IDs this step makes available after success.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        provides_capabilities: Vec<String>,
        /// Generic provider IDs this step makes available after success.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        provides_providers: Vec<String>,
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
        /// Generic capability IDs required before this dependency step runs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requires_capabilities: Vec<String>,
        /// Generic provider IDs required before this dependency step runs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requires_providers: Vec<String>,
        /// Generic capability IDs this step makes available after success.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        provides_capabilities: Vec<String>,
        /// Generic provider IDs this step makes available after success.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        provides_providers: Vec<String>,
        /// Human-readable label shown during execution. If absent, `cmd` is used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Declarative local requirement for common rig preflight checks.
    ///
    /// Use this for simple filesystem/component-path/executable assertions
    /// instead of inline shell. When `prepare_command` is set, it runs only in
    /// listed `prepare_phases`; normal `check` remains read-only and reports
    /// the declared remediation.
    Requirement {
        /// Optional stable node ID for dependency-aware pipeline ordering.
        #[serde(default, rename = "id", skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        /// Step IDs that must run before this step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<String>,
        /// Existing path. Relative paths resolve against `cwd` when set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Existing regular file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        /// Existing directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
        /// Component whose resolved path must contain `component_path_contains`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        component: Option<String>,
        /// Required substring in the resolved component path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        component_path_contains: Option<String>,
        /// Executable name or path. Bare names are resolved from PATH.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executable: Option<String>,
        /// Environment variable whose value overrides `executable` lookup.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executable_env: Option<String>,
        /// Additional environment variables to try, in order, before PATH lookup.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        executable_env_aliases: Vec<String>,
        /// Command to run when the filesystem requirement is missing and the
        /// current pipeline name appears in `prepare_phases`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prepare_command: Option<String>,
        /// Pipeline names allowed to run `prepare_command`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prepare_phases: Vec<String>,
        /// Working directory for `prepare_command` and relative paths.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Env vars (merged over inherited environment) for `prepare_command`.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
        /// Generic capability IDs required before this dependency step runs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requires_capabilities: Vec<String>,
        /// Generic provider IDs required before this dependency step runs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requires_providers: Vec<String>,
        /// Generic capability IDs this step makes available after success.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        provides_capabilities: Vec<String>,
        /// Generic provider IDs this step makes available after success.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        provides_providers: Vec<String>,
        /// Human remediation shown when the requirement is not satisfied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remediation: Option<String>,
        /// Human-readable label shown during execution.
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
