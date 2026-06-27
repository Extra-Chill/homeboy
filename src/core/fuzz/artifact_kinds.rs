//! Canonical fuzz artifact-kind identifiers and alias resolution.
//!
//! Runners, gate evaluation, and result-envelope recognition all refer to the
//! same small set of contract artifacts. Historically each call site carried
//! its own list of spelling aliases (`result-envelope`, `fuzz_result_envelope`,
//! `result_envelope`, ...). This module is the single source of truth: one
//! canonical identifier per contract artifact plus a resolver that folds every
//! accepted spelling onto it, so the contract stays authoritative and generic
//! (#6766).

use crate::core::defaults::extension_provided_fuzz_case_evidence_kinds;

/// Canonical artifact kind for the fuzz result envelope.
pub const FUZZ_ARTIFACT_KIND_RESULT_ENVELOPE: &str = "result_envelope";
/// Canonical artifact kind for the case-level execution log.
pub const FUZZ_ARTIFACT_KIND_CASE_LOG: &str = "case_log";
/// Canonical artifact kind for the coverage summary.
pub const FUZZ_ARTIFACT_KIND_COVERAGE_SUMMARY: &str = "coverage_summary";
/// Canonical artifact kind for replay/reproduction data.
pub const FUZZ_ARTIFACT_KIND_REPLAY_DATA: &str = "replay_data";

/// The canonical contract artifact kinds, in declaration order. Published in the
/// core fuzz contract so consumers (e.g. the WordPress fuzz runner extension)
/// can map their artifact roles onto core's identifiers instead of
/// re-declaring alias arrays by hand.
pub fn canonical_fuzz_artifact_kinds() -> Vec<String> {
    [
        FUZZ_ARTIFACT_KIND_RESULT_ENVELOPE,
        FUZZ_ARTIFACT_KIND_CASE_LOG,
        FUZZ_ARTIFACT_KIND_COVERAGE_SUMMARY,
        FUZZ_ARTIFACT_KIND_REPLAY_DATA,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Fold an artifact-kind spelling onto its canonical contract identifier.
///
/// Normalization lowercases, treats `-` and `_` as equivalent, and strips a
/// leading `fuzz` segment, so `fuzz-result-envelope`, `fuzz_result_envelope`,
/// `result-envelope`, and `result_envelope` all resolve to the same canonical
/// kind. Returns `None` for kinds outside the core contract set.
pub fn canonical_fuzz_artifact_kind(raw: &str) -> Option<&'static str> {
    match normalize_artifact_kind(raw).as_str() {
        "result_envelope" => Some(FUZZ_ARTIFACT_KIND_RESULT_ENVELOPE),
        "case_log" => Some(FUZZ_ARTIFACT_KIND_CASE_LOG),
        "coverage_summary" => Some(FUZZ_ARTIFACT_KIND_COVERAGE_SUMMARY),
        "replay_data" => Some(FUZZ_ARTIFACT_KIND_REPLAY_DATA),
        _ => None,
    }
}

/// True when an artifact-kind spelling names the same canonical contract kind
/// as `canonical`.
pub fn fuzz_artifact_kind_matches(raw: &str, canonical: &str) -> bool {
    match canonical_fuzz_artifact_kind(canonical) {
        Some(target) => canonical_fuzz_artifact_kind(raw) == Some(target),
        None => false,
    }
}

/// Artifact kinds that count as case-level proof for the `has-case-evidence`
/// gate. The canonical `case_log` kind is always recognized; additional
/// ecosystem-specific spellings come from the extension-provided defaults asset
/// so core Rust carries no domain artifact vocabulary (#6766).
pub fn fuzz_case_evidence_artifact_kinds() -> Vec<String> {
    let mut kinds = vec![FUZZ_ARTIFACT_KIND_CASE_LOG.to_string()];
    for kind in extension_provided_fuzz_case_evidence_kinds() {
        let trimmed = kind.trim();
        if !trimmed.is_empty() && !kinds.iter().any(|existing| existing == trimmed) {
            kinds.push(trimmed.to_string());
        }
    }
    kinds
}

fn normalize_artifact_kind(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase().replace('-', "_");
    lowered
        .strip_prefix("fuzz_")
        .map(str::to_string)
        .unwrap_or(lowered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_collapses_result_envelope_aliases() {
        for alias in [
            "result_envelope",
            "result-envelope",
            "fuzz_result_envelope",
            "fuzz-result-envelope",
            "Fuzz-Result-Envelope",
        ] {
            assert_eq!(
                canonical_fuzz_artifact_kind(alias),
                Some(FUZZ_ARTIFACT_KIND_RESULT_ENVELOPE),
                "alias {alias} should resolve to the canonical result-envelope kind",
            );
        }
    }

    #[test]
    fn resolver_collapses_case_log_and_coverage_aliases() {
        assert_eq!(
            canonical_fuzz_artifact_kind("case-log"),
            Some(FUZZ_ARTIFACT_KIND_CASE_LOG),
        );
        assert_eq!(
            canonical_fuzz_artifact_kind("case_log"),
            Some(FUZZ_ARTIFACT_KIND_CASE_LOG),
        );
        assert_eq!(
            canonical_fuzz_artifact_kind("coverage-summary"),
            Some(FUZZ_ARTIFACT_KIND_COVERAGE_SUMMARY),
        );
    }

    #[test]
    fn resolver_rejects_unknown_kinds() {
        assert_eq!(canonical_fuzz_artifact_kind("runner-output"), None);
        assert_eq!(canonical_fuzz_artifact_kind("fuzz_case"), None);
        assert_eq!(canonical_fuzz_artifact_kind(""), None);
    }

    #[test]
    fn kind_matches_across_spellings() {
        assert!(fuzz_artifact_kind_matches(
            "fuzz-result-envelope",
            "result_envelope"
        ));
        assert!(fuzz_artifact_kind_matches("case-log", "case_log"));
        assert!(!fuzz_artifact_kind_matches("case-log", "result_envelope"));
        assert!(!fuzz_artifact_kind_matches("runner-output", "case_log"));
    }

    #[test]
    fn case_evidence_kinds_always_include_canonical_case_log() {
        let kinds = fuzz_case_evidence_artifact_kinds();
        assert_eq!(kinds.first().map(String::as_str), Some("case_log"));
        // No duplicate canonical entry even if the asset repeats it.
        assert_eq!(kinds.iter().filter(|kind| *kind == "case_log").count(), 1);
    }
}
