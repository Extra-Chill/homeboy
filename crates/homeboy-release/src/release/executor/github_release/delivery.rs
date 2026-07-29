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
    /// Unpublished Draft ready to publish, either because its assets were
    /// already uploaded or because the component does not declare an artifact.
    PublishDraft,
    /// Unpublished Draft for an artifact-expecting component with no assets and
    /// nothing to attach. Publishing here would mark an incomplete release
    /// `latest`, so fail loudly and hand the operator the repair commands.
    EmptyDraft,
    /// This run carries artifacts. Reconcile and verify the assets first; the
    /// publish decision is made only after verification succeeds.
    ReconcileAssets,
}

/// Classify an already-existing GitHub Release.
///
/// `has_artifacts` is whether *this run* resolved release artifacts to attach;
/// `existing_asset_count` is how many assets the release already carries;
/// `expects_artifacts` reflects whether the component declares a build artifact.
pub(crate) fn existing_release_action(
    is_draft: bool,
    has_artifacts: bool,
    existing_asset_count: usize,
    expects_artifacts: bool,
) -> ExistingReleaseAction {
    if has_artifacts {
        return ExistingReleaseAction::ReconcileAssets;
    }
    if !is_draft {
        return ExistingReleaseAction::AlreadyPublished;
    }
    if existing_asset_count == 0 && expects_artifacts {
        return ExistingReleaseAction::EmptyDraft;
    }
    ExistingReleaseAction::PublishDraft
}

/// Adoption revalidates the remote inventory immediately before this decision.
/// A concurrent publisher may have completed the same release in that window.
pub(crate) fn adopted_release_needs_publish(is_draft: bool) -> bool {
    is_draft
}
