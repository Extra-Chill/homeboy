//! Fix-plan access for the audit engine, inverted behind a provider.
//!
//! Audit reports a *fixability* summary (how many findings an automated fixer
//! could address) alongside its findings. Computing that requires planning the
//! actual fixes — which is the refactor engine's job. Audit used to call
//! `crate::refactor::plan::generate::*` and `crate::refactor::auto::*` directly,
//! which coupled `code_audit` up to the `refactor` feature layer and formed a
//! `code_audit`↔`refactor` cycle (refactor already depends on audit for the
//! `CodeAuditResult` it fixes).
//!
//! Instead, audit defines the slim view it needs (a per-finding automation
//! verdict) plus a provider trait; the refactor layer registers an
//! implementation at startup (same pattern as the extension-manifest /
//! runner-evidence / tunnel provider hooks). When no provider is registered —
//! e.g. audit running standalone — the no-op provider yields no fixes, which the
//! caller already treats as "not fixable / fixability unavailable".

use std::sync::Mutex;

use super::fingerprint::FileFingerprint;
use super::CodeAuditResult;
use homeboy_audit_contract::AuditFinding;

/// One planned fix's automation verdict, projected for audit's fixability tally.
///
/// Audit never sees the refactor engine's `FixResult` / insertion types; it only
/// needs, per planned fix, which finding it addresses and whether an automated
/// fixer could apply it unattended.
#[derive(Debug, Clone)]
pub struct FixabilityVerdict {
    /// The audit finding this planned fix addresses.
    pub finding: AuditFinding,
    /// Whether an automated fixer could apply this fix without human review.
    pub auto_apply: bool,
}

/// The fix-planning contract the audit engine depends on. Implemented by the
/// refactor layer and registered at startup; audit calls it without depending
/// on refactor behavior.
pub trait AuditFixabilityProvider: Send + Sync {
    /// Plan the fixes for an audit result and project each into a verdict.
    ///
    /// `fingerprints` is the audit run's already-computed fingerprint set (empty
    /// when unavailable); providers may use it to avoid re-fingerprinting.
    /// Returns the per-fix verdicts (empty when nothing is planned).
    fn plan(
        &self,
        result: &CodeAuditResult,
        source_path: &str,
        fingerprints: &[FileFingerprint],
    ) -> Vec<FixabilityVerdict>;
}

/// Default provider used when no refactor layer is registered: no fixes, so the
/// audit engine reports no fixability (exactly as it does when the refactor
/// engine is unavailable).
struct NoopProvider;

impl AuditFixabilityProvider for NoopProvider {
    fn plan(
        &self,
        _result: &CodeAuditResult,
        _source_path: &str,
        _fingerprints: &[FileFingerprint],
    ) -> Vec<FixabilityVerdict> {
        Vec::new()
    }
}

static PROVIDER: Mutex<Option<Box<dyn AuditFixabilityProvider>>> = Mutex::new(None);

/// Register the audit fixability provider. Called once at binary startup by the
/// refactor layer (via the CLI). Replaces any previously registered provider.
pub fn register_audit_fixability_provider(provider: Box<dyn AuditFixabilityProvider>) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Plan fixability verdicts for an audit result via the registered provider.
pub(crate) fn plan_fixability(
    result: &CodeAuditResult,
    source_path: &str,
    fingerprints: &[FileFingerprint],
) -> Vec<FixabilityVerdict> {
    with_provider(|p| p.plan(result, source_path, fingerprints))
}

fn with_provider<T>(f: impl FnOnce(&dyn AuditFixabilityProvider) -> T) -> T {
    let guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(provider) => f(provider.as_ref()),
        None => f(&NoopProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_provider_plans_nothing() {
        let result = CodeAuditResult {
            component_id: "c".into(),
            source_path: "/nonexistent".into(),
            summary: homeboy_audit_contract::AuditSummary {
                files_scanned: 0,
                conventions_detected: 0,
                outliers_found: 0,
                alignment_score: None,
                files_skipped: 0,
                warnings: Vec::new(),
            },
            conventions: Vec::new(),
            directory_conventions: Vec::new(),
            findings: Vec::new(),
            duplicate_groups: Vec::new(),
        };
        assert!(NoopProvider.plan(&result, "/nonexistent", &[]).is_empty());
    }
}
