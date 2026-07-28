//! Publication-state gating for a GitHub Release that already exists.
//!
//! Issue #10441: the release tag becomes durable on `origin` several jobs
//! before the GitHub Release is published. That ordering is not accidental and
//! it is not invertible: `release.yml`'s cargo-dist matrix checks the tag out
//! (`actions/checkout` with `ref: <release-tag>`) and builds with
//! `dist build --tag=<release-tag>`, so no publishable artifact can exist until
//! the tag does. The window between "tag pushed" and "release published" spans
//! the whole cross-platform build, and anything that dies inside it leaves the
//! tag behind with either no release at all or an unpublished Draft.
//!
//! `github.release` is the last step in the pipeline that can close that gap.
//! It therefore must never report success while the release for the tag it was
//! handed is still unpublished — a Draft is a tag that ships nothing, which is
//! precisely the state this issue is about.
//!
//! The decision is factored out as a pure function so the rules are testable
//! without a live `gh`.

/// What `github.release` must do before it may report success for a tag that
/// already carries a GitHub Release object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExistingReleaseAction {
    /// Published release and nothing to attach in this run: a genuine
    /// idempotent no-op.
    AlreadyPublished,
    /// Unpublished Draft carrying assets, with nothing further to attach. The
    /// artifacts were already uploaded (by this pipeline's earlier publish
    /// stage or a previous attempt); publishing is the only remaining action
    /// that makes the pushed tag deliverable.
    PublishDraft,
    /// Unpublished Draft with no assets, and nothing to attach. Publishing here
    /// would mark an empty release `latest` and point every downloader at a
    /// release with no binaries — strictly worse than the Draft. Fail loudly
    /// instead and hand the operator the repair commands.
    EmptyDraft,
    /// This run carries artifacts. Reconcile and verify the assets first; the
    /// publish decision is made only after verification succeeds.
    ReconcileAssets,
}

/// Classify an already-existing GitHub Release.
///
/// `has_artifacts` is whether *this run* resolved release artifacts to attach;
/// `existing_asset_count` is how many assets the release already carries.
pub(crate) fn existing_release_action(
    is_draft: bool,
    has_artifacts: bool,
    existing_asset_count: usize,
) -> ExistingReleaseAction {
    if has_artifacts {
        return ExistingReleaseAction::ReconcileAssets;
    }
    if !is_draft {
        return ExistingReleaseAction::AlreadyPublished;
    }
    if existing_asset_count == 0 {
        return ExistingReleaseAction::EmptyDraft;
    }
    ExistingReleaseAction::PublishDraft
}
