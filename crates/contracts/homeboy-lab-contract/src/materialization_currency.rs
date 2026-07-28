//! One verdict type for "is the copy over there still the copy we made?".
//!
//! # Why this exists
//!
//! Homeboy materializes a local thing onto a remote host in many shapes:
//! workspaces, agent-runtime generations, extension overlays, rig package
//! sources, command-argument files, built artifact directories. Each one grew
//! its own answer to a currency question, and each one picked its own
//! representation for the answer: a `bool`, a bare `Result`, a triple of
//! free-form status strings, or an `Option<u64>` count. They disagree most
//! sharply on the case that matters — what to report when the evidence needed
//! for a verdict could not be gathered at all.
//!
//! This module owns three things and nothing else:
//!
//! 1. [`Currency`] — the verdict, with `Unknown` as a first-class outcome
//!    rather than something that collapses into `Current`.
//! 2. [`CurrencyEvidence`] — what a verdict rests on, and in particular
//!    whether that evidence can prove byte equality
//!    ([`CurrencyEvidence::proves_content_equality`]) or only provenance
//!    equality.
//! 3. Comparators ([`compare_content_digests`], [`compare_source_revisions`],
//!    [`compare_identities`]) that all fail *closed* to `Unknown` when either
//!    side of the comparison is missing.
//!
//! # What this deliberately does not do
//!
//! It does not make content digest the sole authority. Several call sites
//! cannot obtain a digest of the remote side at all: they interrogate a remote
//! installation over a control-plane probe that returns a revision string and
//! nothing else. For those, revision comparison is not a weaker implementation
//! of digest comparison — it is the only evidence that exists. Forcing them
//! onto a digest contract would either require a full remote tree transfer per
//! check or silently disable the check. [`CurrencyEvidence`] records the
//! difference instead of erasing it.
//!
//! # Questions that look alike but are not
//!
//! Adopting this contract is only correct for *identity* questions — "does the
//! copy equal the original". These neighbouring questions share the word
//! "stale" and must not be folded in:
//!
//! - **Derivation currency.** "Is this built artifact behind the source it was
//!   compiled from?" The artifact and its source are different bytes by
//!   construction, so no digest comparison between them can answer it. It is
//!   only answerable by evidence recorded *at build time* by the build step,
//!   which Homeboy does not run.
//! - **Process/build identity.** "Is the daemon serving this host the same
//!   build as the controller?" A running process is not a copied tree; the
//!   only evidence is what the process reports about itself.
//! - **Upstream currency.** "Is this checkout behind its remote tracking
//!   branch?" That compares one tree against a ref it has never held, not
//!   against a copy that was made from it.
//! - **Lifecycle eligibility.** "Has this materialization outlived its
//!   retention policy?" Age, not equality.
//! - **Work scope.** "Which subset of a component changed since a ref?" A
//!   user-supplied scope filter that answers no currency question at all.
//!
//! # Content addressing dissolves rather than answers the question
//!
//! Several materializers name the remote path after the digest of what they
//! put there. Those sites have no verdict to unify, because re-materializing
//! changed content lands at a different path and the old path is still exactly
//! what it always was. [`Currency`] is for sites that compare; it is not
//! something a content-addressed materializer should be retrofitted with.

use serde::{Deserialize, Serialize};

/// What a [`Currency`] verdict rests on.
///
/// Recorded alongside a verdict so a consumer can tell a proof from an
/// indication. Two verdicts of `Current` are not equally strong if one came
/// from a digest and the other from a revision string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrencyEvidence {
    /// A digest computed over the bytes of both sides. The only evidence that
    /// proves the two copies are byte-identical.
    ContentDigest,
    /// A source-control revision reported by each side. Proves both sides were
    /// materialized from the same commit; does not prove either side still
    /// holds those bytes, because a revision can be reported by a tree that has
    /// since been edited.
    SourceRevision,
    /// A version or build-identity string a running program reports about
    /// itself. Applies to processes, not to copied trees.
    BuildIdentity,
    /// A filesystem timestamp compared against source-control history. A
    /// heuristic: timestamps are set by transport and checkout, not only by
    /// builds, so this evidence can be actively misleading and a verdict drawn
    /// from it should prefer [`Currency::Unknown`] over a confident answer.
    BuildTimestamp,
}

impl CurrencyEvidence {
    /// Stable tag for records and diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContentDigest => "content_digest",
            Self::SourceRevision => "source_revision",
            Self::BuildIdentity => "build_identity",
            Self::BuildTimestamp => "build_timestamp",
        }
    }

    /// Whether a `Current` verdict drawn from this evidence proves the two
    /// sides hold the same bytes.
    ///
    /// This is the distinction the codebase kept losing: a revision match and a
    /// digest match were both stored as `true`, so a caller could not tell
    /// "these are the same bytes" from "these claim the same provenance".
    pub const fn proves_content_equality(self) -> bool {
        matches!(self, Self::ContentDigest)
    }
}

/// The verdict for one currency comparison.
///
/// `Unknown` exists so that "the evidence could not be gathered" has somewhere
/// to go other than `Current`. Every fail-open bug this contract is meant to
/// prevent has the same shape: a probe fails, the failure is discarded into an
/// `Option`/`bool`, and the absence is then read as agreement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Currency {
    /// The two sides agree under the recorded evidence.
    Current,
    /// The two sides provably disagree.
    Stale { reason: String },
    /// The comparison could not be made. Not an assertion that the copy is
    /// good, and not an assertion that it is bad.
    Unknown { reason: String },
}

impl Currency {
    /// Build a `Stale` verdict.
    pub fn stale(reason: impl Into<String>) -> Self {
        Self::Stale {
            reason: reason.into(),
        }
    }

    /// Build an `Unknown` verdict.
    pub fn unknown(reason: impl Into<String>) -> Self {
        Self::Unknown {
            reason: reason.into(),
        }
    }

    /// Stable tag for records and diagnostics.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale { .. } => "stale",
            Self::Unknown { .. } => "unknown",
        }
    }

    /// Whether the comparison affirmatively succeeded.
    ///
    /// Deliberately false for `Unknown`, so `is_current()` can never be the
    /// place a missing probe turns into a clean bill of health.
    pub fn is_current(&self) -> bool {
        matches!(self, Self::Current)
    }

    /// Whether the comparison affirmatively failed.
    ///
    /// Also false for `Unknown`: a caller that needs to distinguish "known bad"
    /// from "unproven" gets two separate predicates rather than one negation.
    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Stale { .. })
    }

    /// Whether the comparison could not be made.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown { .. })
    }

    /// The explanation attached to a non-`Current` verdict.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Current => None,
            Self::Stale { reason } | Self::Unknown { reason } => Some(reason.as_str()),
        }
    }
}

/// The identity of one materialized thing, as recorded by whoever materialized
/// it.
///
/// `source_revision` and `dirty` are provenance: they describe where the bytes
/// came from. `algorithm` and `digest` are identity: they describe the bytes.
/// [`compare_identities`] uses only the identity fields, so provenance can be
/// carried in a record without silently becoming a comparison input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedIdentity {
    /// Names the digest construction, not just the hash function. Two digests
    /// computed by different tree-walk rules are not comparable even when both
    /// are SHA-256.
    pub algorithm: String,
    /// The digest itself.
    pub digest: String,
    /// Descriptive provenance. Never a comparison input for
    /// [`compare_identities`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    /// Whether the source had uncommitted modifications when this was
    /// materialized. Descriptive provenance; it explains why a revision is not
    /// sufficient identity, and is not itself a verdict.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
}

impl MaterializedIdentity {
    /// A record carrying identity only.
    pub fn new(algorithm: impl Into<String>, digest: impl Into<String>) -> Self {
        Self {
            algorithm: algorithm.into(),
            digest: digest.into(),
            source_revision: None,
            dirty: false,
        }
    }

    /// Attach descriptive provenance.
    pub fn with_provenance(mut self, source_revision: Option<String>, dirty: bool) -> Self {
        self.source_revision = source_revision;
        self.dirty = dirty;
        self
    }
}

/// Compare two full identities.
///
/// `recorded` is the identity written down when the copy was made; `observed`
/// is what a probe reports now. The two-name framing is deliberate: not every
/// call site is a local/remote pair. A workspace verification compares a
/// controller declaration against a runner rehash, and an installed-package
/// check compares install-time metadata against the source as it stands today.
///
/// Returns `Unknown` when the two sides were computed under different
/// algorithms: unequal digests from different constructions are not evidence of
/// drift, and equal ones would be a coincidence rather than a proof.
pub fn compare_identities(
    subject: &str,
    recorded: &MaterializedIdentity,
    observed: &MaterializedIdentity,
) -> Currency {
    if recorded.algorithm != observed.algorithm {
        return Currency::unknown(format!(
            "{subject} identities are not comparable: recorded algorithm `{}` and observed algorithm `{}` differ",
            recorded.algorithm, observed.algorithm
        ));
    }
    compare_content_digests(
        subject,
        Some(recorded.digest.as_str()),
        Some(observed.digest.as_str()),
    )
}

/// Compare a recorded and observed content digest.
///
/// Evidence: [`CurrencyEvidence::ContentDigest`]. Absent or blank on either
/// side yields `Unknown`.
pub fn compare_content_digests(
    subject: &str,
    recorded: Option<&str>,
    observed: Option<&str>,
) -> Currency {
    compare_opaque(
        subject,
        CurrencyEvidence::ContentDigest,
        "content digest",
        recorded,
        observed,
    )
}

/// Compare a recorded and observed source revision.
///
/// Evidence: [`CurrencyEvidence::SourceRevision`]. A `Current` verdict here
/// proves shared provenance, not byte equality — see
/// [`CurrencyEvidence::proves_content_equality`].
pub fn compare_source_revisions(
    subject: &str,
    recorded: Option<&str>,
    observed: Option<&str>,
) -> Currency {
    compare_opaque(
        subject,
        CurrencyEvidence::SourceRevision,
        "source revision",
        recorded,
        observed,
    )
}

/// Shared comparison for two opaque identity strings.
///
/// Blank is treated as absent, because every call site that reached for this
/// had already learned that a probe can return an empty string instead of
/// failing, and each of them filtered for it separately.
fn compare_opaque(
    subject: &str,
    evidence: CurrencyEvidence,
    label: &str,
    recorded: Option<&str>,
    observed: Option<&str>,
) -> Currency {
    let recorded = recorded.map(str::trim).filter(|value| !value.is_empty());
    let observed = observed.map(str::trim).filter(|value| !value.is_empty());
    match (recorded, observed) {
        (Some(recorded), Some(observed)) if recorded == observed => Currency::Current,
        (Some(recorded), Some(observed)) => Currency::stale(format!(
            "{subject}: recorded {label} {recorded} differs from observed {label} {observed} (evidence: {})",
            evidence.as_str()
        )),
        (None, Some(_)) => Currency::unknown(format!(
            "{subject}: currency could not be determined because the recorded {label} was unavailable"
        )),
        (Some(_), None) => Currency::unknown(format!(
            "{subject}: currency could not be determined because the observed {label} was unavailable"
        )),
        (None, None) => Currency::unknown(format!(
            "{subject}: currency could not be determined because no {label} was available on either side"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compare_content_digests, compare_identities, compare_source_revisions, Currency,
        CurrencyEvidence, MaterializedIdentity,
    };

    #[test]
    fn missing_evidence_is_unknown_rather_than_current() {
        // The single behaviour this contract exists to force. Every mechanism
        // it replaces had some path where a failed probe produced the same
        // value as a successful match.
        for verdict in [
            compare_content_digests("workspace", None, Some("sha256:a")),
            compare_content_digests("workspace", Some("sha256:a"), None),
            compare_content_digests("workspace", None, None),
            compare_source_revisions("extension", None, Some("abc1234")),
            compare_source_revisions("extension", Some("abc1234"), None),
            compare_source_revisions("extension", None, None),
        ] {
            assert!(verdict.is_unknown(), "expected unknown, got {verdict:?}");
            assert!(!verdict.is_current());
            assert!(!verdict.is_stale());
        }
    }

    #[test]
    fn blank_evidence_counts_as_absent_on_either_side() {
        assert!(compare_source_revisions("rig", Some("   "), Some("abc1234")).is_unknown());
        assert!(compare_source_revisions("rig", Some("abc1234"), Some("")).is_unknown());
        assert!(compare_content_digests("workspace", Some(""), Some("")).is_unknown());
    }

    #[test]
    fn matching_and_differing_evidence_produce_the_two_decided_verdicts() {
        assert_eq!(
            compare_content_digests("workspace", Some("sha256:a"), Some("sha256:a")),
            Currency::Current
        );
        let stale = compare_content_digests("workspace", Some("sha256:a"), Some("sha256:b"));
        assert!(stale.is_stale());
        assert!(stale.reason().unwrap_or_default().contains("sha256:b"));
        assert!(stale
            .reason()
            .unwrap_or_default()
            .contains("content_digest"));
    }

    #[test]
    fn digest_evidence_proves_byte_equality_and_revision_evidence_does_not() {
        // A revision match and a digest match were both stored as `true` in the
        // mechanisms this replaces, which is exactly why a dirty worktree could
        // satisfy one check and fail another for the same subject.
        assert!(CurrencyEvidence::ContentDigest.proves_content_equality());
        assert!(!CurrencyEvidence::SourceRevision.proves_content_equality());
        assert!(!CurrencyEvidence::BuildIdentity.proves_content_equality());
        assert!(!CurrencyEvidence::BuildTimestamp.proves_content_equality());
    }

    #[test]
    fn identities_computed_under_different_algorithms_are_not_comparable() {
        let local = MaterializedIdentity::new("homeboy-workspace-content-v1", "sha256:a");
        let remote = MaterializedIdentity::new("homeboy-workspace-content-v2+portable", "sha256:a");

        let verdict = compare_identities("workspace", &local, &remote);

        // Equal digest strings under unequal constructions are a coincidence,
        // not a proof, so this must not report Current.
        assert!(verdict.is_unknown());
        assert!(verdict
            .reason()
            .unwrap_or_default()
            .contains("not comparable"));
    }

    #[test]
    fn provenance_fields_are_not_comparison_inputs() {
        let local = MaterializedIdentity::new("alg", "sha256:a")
            .with_provenance(Some("abc1234".to_string()), true);
        let remote = MaterializedIdentity::new("alg", "sha256:a")
            .with_provenance(Some("def5678".to_string()), false);

        // Differing revision and dirty flag, identical bytes: the bytes decide.
        assert_eq!(compare_identities("workspace", &local, &remote), Currency::Current);
    }

    #[test]
    fn verdict_tags_are_stable_for_records() {
        assert_eq!(Currency::Current.tag(), "current");
        assert_eq!(Currency::stale("because").tag(), "stale");
        assert_eq!(Currency::unknown("because").tag(), "unknown");
        assert_eq!(
            serde_json::to_value(Currency::Current).expect("serialize"),
            serde_json::json!({ "verdict": "current" })
        );
        assert_eq!(
            serde_json::to_value(Currency::stale("drifted")).expect("serialize"),
            serde_json::json!({ "verdict": "stale", "reason": "drifted" })
        );
    }
}
