//! Stack primitive — combined-fixes branches as a first-class artifact.
//!
//! A **stack** is a declarative description of a maintenance branch built
//! from an upstream `base` plus an ordered list of `prs` cherry-picked
//! on top:
//!
//! ```text
//! base.remote/base.branch
//!   ↳ cherry-pick PR #1 (head SHA)
//!   ↳ cherry-pick PR #2
//!   ↳ cherry-pick PR #3
//! → target.remote/target.branch
//! ```
//!
//! Phase 1 MVP scope:
//! - Spec schema with `id` / `component` / `component_path` / `base` / `target` / `prs`
//! - State directory at `~/.config/homeboy/stacks/{id}.json` (mirror of rig layout)
//! - CLI verbs: `list`, `show`, `create`, `add-pr`, `remove-pr`, `apply`,
//!   `status`, `inspect`
//! - Pause-and-resume on cherry-pick conflicts (apply exits non-zero with a
//!   clear message; user resolves manually with raw git tools)
//!
//! Deferred to Phase 2+ (Extra-Chill/homeboy#1462):
//! - `sync` (auto-drop merged PRs after rebase) — requires `rebase` first
//! - `rebase`, `push`, `diff`, `continue` / `--reset` resume primitives
//! - Per-PR `--squash` / `--merge` / `--preserve` flags
//! - Conflict resolution cache

pub mod apply;
pub mod inspect;
pub mod spec;
pub mod status;

pub use apply::{apply, AppliedPr, ApplyOutput, PickOutcome};
pub use inspect::{inspect, inspect_at, InspectCommit, InspectOptions, InspectOutput, InspectPr};
pub use spec::{
    exists, expand_path, list, list_ids, load, parse_git_ref, save, GitRef, StackPrEntry, StackSpec,
};
pub use status::{status, LocalState, StatusOutput, StatusPr};
