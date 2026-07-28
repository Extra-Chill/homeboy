//! Tests for the publication-state gating of an already-existing GitHub
//! Release (issue #10441).
//!
//! Live evidence these encode, from `Extra-Chill/homeboy` on 2026-07-28:
//!
//! | tag       | git tag on origin | GitHub Release        |
//! |-----------|-------------------|-----------------------|
//! | `v0.321.1`| present           | Draft, 13 assets      |
//! | `v0.321.0`| present           | published (Latest)    |
//! | `v0.320.0`| present           | Draft, 15 assets      |
//! | `v0.319.3`| present           | none                  |
//!
//! Every Draft row is a pushed tag that ships nothing. Before this change the
//! `github.release` step returned `Success` with `skipped: true` for all of
//! them whenever the run carried no artifacts of its own, so the pipeline
//! reported a delivered release over an undeliverable tag.

use super::super::{existing_release_action, ExistingReleaseAction};

#[test]
fn published_release_with_nothing_to_attach_is_an_idempotent_no_op() {
    assert_eq!(
        existing_release_action(false, false, 13, true),
        ExistingReleaseAction::AlreadyPublished
    );
}

#[test]
fn published_release_with_no_assets_is_still_an_idempotent_no_op() {
    // Asset count is only consulted for drafts: a published release with no
    // assets is a delivery problem for `verify-published` to report, not a
    // reason for this step to re-publish something already public.
    assert_eq!(
        existing_release_action(false, false, 0, true),
        ExistingReleaseAction::AlreadyPublished
    );
}

#[test]
fn stranded_draft_carrying_assets_is_published_not_skipped() {
    // The v0.320.0 / v0.321.1 state: the tag is durable on origin, cargo-dist
    // already uploaded every asset, and only the un-draft edit is missing.
    // Reporting `skipped` here is what let those tags sit undeliverable.
    assert_eq!(
        existing_release_action(true, false, 15, true),
        ExistingReleaseAction::PublishDraft
    );
    assert_eq!(
        existing_release_action(true, false, 1, true),
        ExistingReleaseAction::PublishDraft
    );
}

#[test]
fn empty_draft_for_artifact_component_is_refused_rather_than_published() {
    // Publishing an assetless draft marks it `latest` and points every
    // downloader (and the Homebrew formula) at a release with no binaries.
    // That is strictly worse than leaving the draft for the recovery path,
    // which can still upload artifacts into it.
    assert_eq!(
        existing_release_action(true, false, 0, true),
        ExistingReleaseAction::EmptyDraft
    );
}

#[test]
fn empty_draft_for_assetless_component_is_published() {
    assert_eq!(
        existing_release_action(true, false, 0, false),
        ExistingReleaseAction::PublishDraft
    );
}

#[test]
fn a_run_carrying_artifacts_always_reconciles_before_any_publish_decision() {
    // Assets must be uploaded and verified by digest before the publish
    // decision is made, so the artifact-carrying run never short-circuits into
    // a publish — regardless of draft state or how many assets already exist.
    for is_draft in [true, false] {
        for existing in [0usize, 1, 15] {
            assert_eq!(
                existing_release_action(is_draft, true, existing, true),
                ExistingReleaseAction::ReconcileAssets,
                "is_draft={is_draft} existing_assets={existing}"
            );
        }
    }
}

#[test]
fn no_input_combination_reports_a_draft_as_already_published() {
    // The core invariant of issue #10441: a durable tag must never be reported
    // as delivered while its release is an unpublished draft.
    for existing in [0usize, 1, 13, 15] {
        for has_artifacts in [true, false] {
            assert_ne!(
                existing_release_action(true, has_artifacts, existing, true),
                ExistingReleaseAction::AlreadyPublished,
                "a draft must never classify as published (has_artifacts={has_artifacts}, existing_assets={existing})"
            );
        }
    }
}
