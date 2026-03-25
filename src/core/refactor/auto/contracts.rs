mod fix_result;
mod helpers;
mod insertion_kind;
mod policy_summary;
mod types;

pub use fix_result::*;
pub use helpers::*;
pub use insertion_kind::*;
pub use policy_summary::*;
pub use types::*;

use crate::code_audit::conventions::AuditFinding;
use crate::core::refactor::decompose;
use std::path::Path;

impl InsertionKind {
    pub fn safety_tier(&self) -> FixSafetyTier {
        match self {
            // Safe: all deterministic, mechanical fixes that can be auto-applied.
            // Preflight validation runs when applicable (registration stubs get
            // collision checks, visibility changes get simulation checks, etc).
            Self::ImportAdd
            | Self::DocReferenceUpdate { .. }
            | Self::DocLineRemoval { .. }
            | Self::RegistrationStub
            | Self::ConstructorWithRegistration
            | Self::TypeConformance
            | Self::NamespaceDeclaration
            | Self::VisibilityChange { .. }
            | Self::ReexportRemoval { .. }
            | Self::LineReplacement { .. }
            | Self::FileMove { .. }
            | Self::TestModule => FixSafetyTier::Safe,

            // Plan-only: requires human review.
            Self::MethodStub | Self::FunctionRemoval { .. } | Self::TraitUse => {
                FixSafetyTier::PlanOnly
            }
        }
    }
}

impl FixResult {
    /// Strip generated code from insertions and new files, replacing with byte-count placeholders.
    pub fn strip_code(&mut self) {
        for fix in &mut self.fixes {
            for insertion in &mut fix.insertions {
                let len = insertion.code.len();
                insertion.code = format!("[{len} bytes]");
            }
        }
        for new_file in &mut self.new_files {
            let len = new_file.content.len();
            new_file.content = format!("[{len} bytes]");
        }
    }

    /// Compute a breakdown of finding types and their fix counts.
    pub fn finding_counts(&self) -> std::collections::BTreeMap<AuditFinding, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for fix in &self.fixes {
            for insertion in &fix.insertions {
                *counts.entry(insertion.finding.clone()).or_insert(0) += 1;
            }
        }
        for new_file in &self.new_files {
            *counts.entry(new_file.finding.clone()).or_insert(0) += 1;
        }
        for plan in &self.decompose_plans {
            *counts.entry(plan.source_finding.clone()).or_insert(0) += 1;
        }
        counts
    }
}

impl PolicySummary {
    pub fn has_blocked_items(&self) -> bool {
        self.blocked_insertions > 0 || self.blocked_new_files > 0
    }
}
