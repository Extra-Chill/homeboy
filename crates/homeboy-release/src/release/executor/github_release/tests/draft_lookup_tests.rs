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
    // Pagination can put blank lines before the match.
    let stdout = format!("\n   \n{DRAFT_LINE}\n");
    let metadata = parse_listed_release_metadata(&stdout).expect("draft must resolve");

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
fn draft_verdict_resolves_from_rest_and_graphql_spellings() {
    // `gh release list --json isDraft` uses camelCase; the metadata type
    // aliases it so both shapes resolve to the same verdict.
    for (stdout, is_draft) in [
        (
            r#"{"id":124,"tag_name":"v0.321.0","draft":false,"assets":[]}"#,
            false,
        ),
        (r#"{"tagName":"v0.321.1","isDraft":true}"#, true),
    ] {
        let metadata = parse_listed_release_metadata(stdout).expect("release must resolve");
        assert_eq!(metadata.is_draft, is_draft, "{stdout}");
        assert!(metadata.assets.is_empty());
    }
}

#[test]
fn no_match_or_unparseable_output_yields_none_rather_than_a_wrong_draft_verdict() {
    // `--jq select(...)` prints nothing when no page contains the tag; the
    // caller reports that as "no release matched". Guessing on unparseable
    // output would be worse: defaulting to "published" would report a
    // delivered release over a stranded draft, and defaulting to "draft"
    // would republish a live release.
    for stdout in ["", "\n\n   \n", "not json at all", "{", "[]"] {
        assert!(
            parse_listed_release_metadata(stdout).is_none(),
            "{stdout:?}"
        );
    }
}
