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
//!   `rebase`, `sync`, `status`, `inspect`
//! - Pause-and-resume on cherry-pick conflicts (apply exits non-zero with a
//!   clear message; user resolves manually with raw git tools)
//!
//! Deferred to Phase 2+ (Extra-Chill/homeboy#1462):
//! - `push`, `diff`, `continue` / `--reset` resume primitives
//! - Per-PR `--squash` / `--merge` / `--preserve` flags
//! - Conflict resolution cache

pub mod apply;
pub(crate) mod git;
pub mod inspect;
pub(crate) mod pr_meta;
pub mod push;
pub mod spec;
pub mod status;
pub mod sync;

pub use apply::{apply, rebase, AppliedPr, ApplyOutput, PickOutcome, RebaseOutput};
pub use inspect::{inspect, inspect_at, InspectCommit, InspectOptions, InspectOutput, InspectPr};
pub use push::{push, PushOutput, PushStatus};
pub use spec::{
    exists, expand_path, list, list_ids, load, parse_git_ref, save, GitRef, StackPrEntry, StackSpec,
};
pub use status::{status, LocalState, StatusOutput, StatusPr};
pub use sync::{
    diff, sync, DiffOutput, DroppedPr, ReplayedPr, SyncOutput, SyncPreview, UncertainPr,
};
