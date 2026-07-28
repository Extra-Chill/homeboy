//! Tests for parsing the draft-release fallback lookup (issues #10480, #10441).
//!
//! `GET /repos/{owner}/{repo}/releases/tags/{tag}` returns 404 for a draft, so
//! a release stranded by a failed publish can only be resolved through the list
//! endpoint. Everything downstream of that fallback — asset digests, the
//! draft/published decision, whether recovery can finish the release at all —
//! depends on parsing its output correctly, and the failure is silent when it
//! does not: the release simply stays a draft behind a pushed tag.
//!
//! The fixtures below are shaped exactly like the live output of
//! `gh api --paginate repos/Extra-Chill/homeboy/releases \
//!    --jq '.[] | select(.tag_name == "v0.321.1")'`
//! captured while `v0.321.1` was stranded as a Draft with 13 assets: `gh`
//! emits ONE compact JSON object per line, and `--paginate` concatenates
//! pages, so a multi-page scan can put unrelated lines around the match.

use super::super::parse_listed_release_metadata;

/// One compact line, the real shape `gh api --jq` emits for a single match.
const DRAFT_LINE: &str = r#"{"id":123,"tag_name":"v0.321.1","draft":true,"assets":[{"id":492086569,"name":"homeboy-aarch64-unknown-linux-gnu.tar.xz","size":23033568,"digest":"sha256:ed6b93c03f5638a8b70d4c077270145c88a8dc2fea987443bfbd0b82acd93710","state":"uploaded"}]}"#;

#[test]
fn a_stranded_draft_is_resolved_with_its_assets_and_digests() {
    let metadata = parse_listed_release_metadata(DRAFT_LINE).expect("draft must resolve");

    assert!(
        metadata.is_draft,
        "the whole point of the fallback is learning the release is still a draft"
    );
    assert_eq!(metadata.assets.len(), 1);
    assert_eq!(
        metadata.assets[0].name,
        "homeboy-aarch64-unknown-linux-gnu.tar.xz"
    );
    assert_eq!(metadata.assets[0].size, 23_033_568);
    // The REST digest is the reason the fallback reads the API rather than
    // `gh release view --json`, whose GraphQL shape omits it.
    assert_eq!(
        metadata.assets[0].digest.as_deref(),
        Some("sha256:ed6b93c03f5638a8b70d4c077270145c88a8dc2fea987443bfbd0b82acd93710")
    );
    assert_eq!(metadata.assets[0].id, Some(492_086_569));
}

#[test]
fn a_published_release_is_resolved_as_not_draft() {
    let published = r#"{"id":124,"tag_name":"v0.321.0","draft":false,"assets":[]}"#;
    let metadata = parse_listed_release_metadata(published).expect("published must resolve");
    assert!(!metadata.is_draft);
    assert!(metadata.assets.is_empty());
}

#[test]
fn leading_blank_lines_from_pagination_are_skipped() {
    let stdout = format!("\n   \n{DRAFT_LINE}\n");
    let metadata =
        parse_listed_release_metadata(&stdout).expect("blank pages must not hide the match");
    assert!(metadata.is_draft);
    assert_eq!(metadata.assets.len(), 1);
}

#[test]
fn no_match_yields_none_so_the_caller_can_report_it() {
    // `--jq select(...)` prints nothing when no page contains the tag. The
    // caller turns this into "no release matched", which is a different
    // operator action than "the gh call failed".
    assert!(parse_listed_release_metadata("").is_none());
    assert!(parse_listed_release_metadata("\n\n   \n").is_none());
}

#[test]
fn unparseable_output_yields_none_rather_than_a_wrong_draft_verdict() {
    // Guessing here would be worse than failing: defaulting to "published"
    // would let the step report a delivered release over a stranded draft,
    // and defaulting to "draft" would republish a live release.
    assert!(parse_listed_release_metadata("not json at all").is_none());
    assert!(parse_listed_release_metadata("{").is_none());
    assert!(parse_listed_release_metadata("[]").is_none());
}

#[test]
fn the_graphql_is_draft_spelling_is_accepted_too() {
    // `gh release list --json isDraft` uses camelCase; the metadata type
    // aliases it so both shapes resolve to the same verdict.
    let graphql = r#"{"tagName":"v0.321.1","isDraft":true}"#;
    let metadata = parse_listed_release_metadata(graphql).expect("camelCase must resolve");
    assert!(metadata.is_draft);
    assert!(metadata.assets.is_empty());
}
