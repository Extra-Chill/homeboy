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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Unpublished Draft that carries SOME of the assets its own distribution
    /// manifest declares. Publishing it ships a release whose missing platforms
    /// return 404, so it is refused for the same reason as `EmptyDraft`.
    ///
    /// This is the case `existing_asset_count == 0` could never see. `v0.323.1`
    /// published with 2 of 14 assets — both Linux archives absent — because a
    /// non-zero count classified as `PublishDraft`.
    PartialDraft { missing: Vec<String> },
    /// This run carries artifacts. Reconcile and verify the assets first; the
    /// publish decision is made only after verification succeeds.
    ReconcileAssets,
}

/// Classify an already-existing GitHub Release.
///
/// `has_artifacts` is whether *this run* resolved release artifacts to attach;
/// `existing_assets` are the asset names the release already carries;
/// `declared_assets` is the completeness contract the release declares for
/// itself (its own `dist-manifest.json`, or an adoption manifest's expected
/// set) when one could be resolved; `expects_artifacts` reflects whether the
/// component declares a build artifact.
///
/// Taking the declared set rather than only a count is the point. The previous
/// signature received `existing_asset_count: usize` and so could distinguish
/// only "empty" from "non-empty" — it was structurally incapable of seeing a
/// partially uploaded draft, which is how a release with 2 of 14 assets was
/// published (#8687).
pub(crate) fn existing_release_action(
    is_draft: bool,
    has_artifacts: bool,
    existing_assets: &[String],
    declared_assets: Option<&[String]>,
    expects_artifacts: bool,
) -> ExistingReleaseAction {
    if has_artifacts {
        return ExistingReleaseAction::ReconcileAssets;
    }
    if !is_draft {
        return ExistingReleaseAction::AlreadyPublished;
    }
    if existing_assets.is_empty() && expects_artifacts {
        return ExistingReleaseAction::EmptyDraft;
    }
    let missing = missing_declared_assets(existing_assets, declared_assets);
    if !missing.is_empty() {
        return ExistingReleaseAction::PartialDraft { missing };
    }
    ExistingReleaseAction::PublishDraft
}

/// Declared assets absent from `existing`.
///
/// An unresolvable contract (`None`) yields no missing assets: a release whose
/// completeness cannot be established must not be blocked on that basis, or a
/// component that ships no manifest could never publish at all. Refusal is
/// reserved for a contract that exists and is demonstrably unmet.
pub(crate) fn missing_declared_assets(
    existing: &[String],
    declared: Option<&[String]>,
) -> Vec<String> {
    let Some(declared) = declared else {
        return Vec::new();
    };
    declared
        .iter()
        .filter(|name| !existing.iter().any(|present| present == *name))
        .cloned()
        .collect()
}
