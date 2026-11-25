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

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn existing_release_without_run_artifacts_is_classified_by_draft_state_and_contract() {
    use ExistingReleaseAction::{AlreadyPublished, EmptyDraft, PublishDraft};
    let linux = "homeboy-x86_64-unknown-linux-gnu.tar.xz";
    let darwin = "homeboy-aarch64-apple-darwin.tar.xz";

    // (case, is_draft, present assets, declared contract, artifact component, expected)
    let cases: Vec<(
        &str,
        bool,
        Vec<String>,
        Option<Vec<String>>,
        bool,
        ExistingReleaseAction,
    )> = vec![
        // Asset count and contract are only consulted for drafts: a published
        // release is `verify-published`'s problem, never something to re-publish.
        (
            "published with assets",
            false,
            assets(13),
            None,
            true,
            AlreadyPublished,
        ),
        (
            "published without assets",
            false,
            assets(0),
            None,
            true,
            AlreadyPublished,
        ),
        (
            "published missing a declared asset",
            false,
            names(&["source.tar.gz"]),
            Some(names(&[linux])),
            true,
            AlreadyPublished,
        ),
        // The v0.320.0 / v0.321.1 state: assets uploaded, only the un-draft
        // edit is missing. Reporting `skipped` let those tags sit undeliverable.
        (
            "stranded draft with assets",
            true,
            assets(15),
            None,
            true,
            PublishDraft,
        ),
        (
            "stranded draft with one asset",
            true,
            assets(1),
            None,
            true,
            PublishDraft,
        ),
        // Publishing an assetless draft marks it latest and points every
        // downloader at a release with no binaries.
        (
            "empty draft, artifact component",
            true,
            assets(0),
            None,
            true,
            EmptyDraft,
        ),
        (
            "empty draft, assetless component",
            true,
            assets(0),
            None,
            false,
            PublishDraft,
        ),
        // Extra assets beyond the contract (sidecars, source archives, the
        // Homebrew formula) are not a reason to refuse.
        (
            "draft carrying every declared asset",
            true,
            names(&[darwin, linux, "homeboy.rb", "sha256.sum", "source.tar.gz"]),
            Some(names(&[darwin, linux])),
            true,
            PublishDraft,
        ),
        // An unresolvable contract must not make a component unreleasable.
        (
            "draft without a contract",
            true,
            names(&["some-artifact.zip"]),
            None,
            true,
            PublishDraft,
        ),
    ];

    for (case, is_draft, present, declared, artifact_component, expected) in cases {
        assert_eq!(
            existing_release_action(
                is_draft,
                false,
                &present,
                declared.as_deref(),
                artifact_component
            ),
            expected,
            "{case}"
        );
    }

    // No contract, or an empty one, declares nothing missing.
    let present = names(&["anything.zip"]);
    assert!(missing_declared_assets(&present, None).is_empty());
    assert!(missing_declared_assets(&present, Some(&[])).is_empty());
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
