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

use super::super::{existing_release_action, missing_declared_assets, ExistingReleaseAction};

/// `n` asset names, standing in for a release that carries that many assets.
/// The classifier now reads names rather than a count, so tests that only care
/// about "how many" build a set of that size.
fn assets(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("asset-{index}.tar.xz"))
        .collect()
}

#[test]
fn published_release_with_nothing_to_attach_is_an_idempotent_no_op() {
    assert_eq!(
        existing_release_action(false, false, &assets(13), None, true),
        ExistingReleaseAction::AlreadyPublished
    );
}

#[test]
fn published_release_with_no_assets_is_still_an_idempotent_no_op() {
    // Asset count is only consulted for drafts: a published release with no
    // assets is a delivery problem for `verify-published` to report, not a
    // reason for this step to re-publish something already public.
    assert_eq!(
        existing_release_action(false, false, &assets(0), None, true),
        ExistingReleaseAction::AlreadyPublished
    );
}

#[test]
fn stranded_draft_carrying_assets_is_published_not_skipped() {
    // The v0.320.0 / v0.321.1 state: the tag is durable on origin, cargo-dist
    // already uploaded every asset, and only the un-draft edit is missing.
    // Reporting `skipped` here is what let those tags sit undeliverable.
    assert_eq!(
        existing_release_action(true, false, &assets(15), None, true),
        ExistingReleaseAction::PublishDraft
    );
    assert_eq!(
        existing_release_action(true, false, &assets(1), None, true),
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
        existing_release_action(true, false, &assets(0), None, true),
        ExistingReleaseAction::EmptyDraft
    );
}

#[test]
fn empty_draft_for_assetless_component_is_published() {
    assert_eq!(
        existing_release_action(true, false, &assets(0), None, false),
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
                existing_release_action(is_draft, true, &assets(existing), None, true),
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
                existing_release_action(true, has_artifacts, &assets(existing), None, true),
                ExistingReleaseAction::AlreadyPublished,
                "a draft must never classify as published (has_artifacts={has_artifacts}, existing_assets={existing})"
            );
        }
    }
}

/// The v0.323.1 state, which `existing_asset_count == 0` could never see.
///
/// All four platform builds succeeded; the publish step failed after uploading
/// two assets. A non-zero count classified the draft as `PublishDraft`, so it
/// was published with 2 of 14 assets and served 404 for both Linux archives
/// (#8687).
#[test]
fn partially_uploaded_draft_is_refused_rather_than_published() {
    let declared = vec![
        "dist-manifest.json".to_string(),
        "homeboy-aarch64-apple-darwin.tar.xz".to_string(),
        "homeboy-aarch64-unknown-linux-gnu.tar.xz".to_string(),
        "homeboy-x86_64-apple-darwin.tar.xz".to_string(),
        "homeboy-x86_64-unknown-linux-gnu.tar.xz".to_string(),
    ];
    let present = vec![
        "homeboy-aarch64-apple-darwin.tar.xz".to_string(),
        "source.tar.gz".to_string(),
    ];

    let action = existing_release_action(true, false, &present, Some(&declared), true);

    match action {
        ExistingReleaseAction::PartialDraft { missing } => {
            assert!(missing.contains(&"homeboy-x86_64-unknown-linux-gnu.tar.xz".to_string()));
            assert!(missing.contains(&"homeboy-aarch64-unknown-linux-gnu.tar.xz".to_string()));
            assert!(missing.contains(&"dist-manifest.json".to_string()));
            assert!(
                !missing.contains(&"homeboy-aarch64-apple-darwin.tar.xz".to_string()),
                "an asset that IS present must not be reported missing"
            );
        }
        other => panic!("a partially uploaded draft must be refused, got {other:?}"),
    }
}

/// A draft carrying everything its manifest declares still publishes. Extra
/// assets beyond the contract (sidecars, source archives, the Homebrew
/// formula) are not a reason to refuse.
#[test]
fn draft_carrying_every_declared_asset_still_publishes() {
    let declared = vec![
        "homeboy-aarch64-apple-darwin.tar.xz".to_string(),
        "homeboy-x86_64-unknown-linux-gnu.tar.xz".to_string(),
    ];
    let present = vec![
        "homeboy-aarch64-apple-darwin.tar.xz".to_string(),
        "homeboy-x86_64-unknown-linux-gnu.tar.xz".to_string(),
        "homeboy.rb".to_string(),
        "sha256.sum".to_string(),
        "source.tar.gz".to_string(),
    ];

    assert_eq!(
        existing_release_action(true, false, &present, Some(&declared), true),
        ExistingReleaseAction::PublishDraft
    );
}

/// An unresolvable contract must not block publication. A component that ships
/// no distribution manifest has no declared set to compare against, and
/// refusing on that basis would make it unreleasable.
#[test]
fn a_draft_without_a_resolvable_contract_is_not_refused() {
    let present = vec!["some-artifact.zip".to_string()];

    assert_eq!(
        existing_release_action(true, false, &present, None, true),
        ExistingReleaseAction::PublishDraft
    );
    assert!(missing_declared_assets(&present, None).is_empty());
}

/// An empty declared set is a contract that is trivially met, not an
/// unresolvable one.
#[test]
fn an_empty_declared_contract_is_satisfied() {
    let present = vec!["anything.zip".to_string()];
    assert!(missing_declared_assets(&present, Some(&[])).is_empty());
}

/// Refusal is reserved for drafts. A published release missing declared assets
/// is `verify-published`'s problem to report; re-classifying it here would let
/// this step try to re-publish something already public.
#[test]
fn a_published_release_is_never_reclassified_as_partial() {
    let declared = vec!["homeboy-x86_64-unknown-linux-gnu.tar.xz".to_string()];
    let present = vec!["source.tar.gz".to_string()];

    assert_eq!(
        existing_release_action(false, false, &present, Some(&declared), true),
        ExistingReleaseAction::AlreadyPublished
    );
}
