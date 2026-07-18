//! Refactor-side implementation of core's refactor transform hook.
//!
//! The extension test-drift auto-fixer asks the refactor engine to apply a
//! transform set (regex source edits) and format the autofix outcome. This
//! provides that behavior to core through the `RefactorTransformProvider` hook
//! so the extension does not depend on the refactor feature layer.

use std::path::Path;

use homeboy_core::refactor_transform_provider::{
    register_refactor_transform_provider, AppliedTransformSummary, RefactorTransformProvider,
};
use homeboy_core::Result;
use homeboy_refactor_contract::TransformSet;

use crate::auto::{self, AutofixMode};
use crate::transform;

struct RefactorTransformProviderImpl;

impl RefactorTransformProvider for RefactorTransformProviderImpl {
    fn apply_transform_set(
        &self,
        root: &Path,
        name: &str,
        set: &TransformSet,
        write: bool,
        rerun_hint: Option<String>,
        extra_hints: Vec<String>,
    ) -> Result<AppliedTransformSummary> {
        let result = transform::apply_transforms(
            root,
            name,
            set,
            write,
            None,
            Some(transform::DEFAULT_MATCH_DETAIL_LIMIT),
        )?;
        let outcome = auto::standard_outcome(
            if write {
                AutofixMode::Write
            } else {
                AutofixMode::DryRun
            },
            result.total_replacements,
            rerun_hint,
            extra_hints,
        );
        Ok(AppliedTransformSummary {
            total_replacements: result.total_replacements,
            total_files: result.total_files,
            rerun_recommended: outcome.rerun_recommended,
            hints: outcome.hints,
        })
    }
}

/// Register the refactor transform provider so core's extension test-drift
/// auto-fixer can apply generated transform rules without depending on the
/// refactor feature layer.
pub fn register() {
    register_refactor_transform_provider(Box::new(RefactorTransformProviderImpl));
}
