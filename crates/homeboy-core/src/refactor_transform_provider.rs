//! Refactor transform-application hook.
//!
//! The extension test-drift auto-fixer generates a set of transform rules and
//! applies them to a component's sources. Applying transforms (regex-based
//! source edits) and formatting the autofix outcome is the refactor engine's
//! job, so it is inverted behind this provider: the extension layer owns drift
//! detection and rule generation, the refactor layer applies the transforms.
//!
//! With no provider registered (no refactor layer present) the no-op applies
//! nothing.

use std::path::Path;
use std::sync::Mutex;

use homeboy_refactor_contract::TransformSet;

use crate::Result;

/// The slim result of applying a transform set, as the extension test-drift
/// workflow consumes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppliedTransformSummary {
    pub total_replacements: usize,
    pub total_files: usize,
    pub rerun_recommended: bool,
    pub hints: Vec<String>,
}

/// Applies a transform set to a component's sources.
pub trait RefactorTransformProvider: Send + Sync {
    /// Apply `set` under `root`, writing changes when `write` is true, and
    /// return a slim summary. `rerun_hint` / `extra_hints` shape the outcome
    /// hints the caller surfaces.
    fn apply_transform_set(
        &self,
        root: &Path,
        name: &str,
        set: &TransformSet,
        write: bool,
        rerun_hint: Option<String>,
        extra_hints: Vec<String>,
    ) -> Result<AppliedTransformSummary>;
}

struct NoopProvider;

impl RefactorTransformProvider for NoopProvider {
    fn apply_transform_set(
        &self,
        _root: &Path,
        _name: &str,
        _set: &TransformSet,
        _write: bool,
        _rerun_hint: Option<String>,
        _extra_hints: Vec<String>,
    ) -> Result<AppliedTransformSummary> {
        Ok(AppliedTransformSummary::default())
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn RefactorTransformProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn RefactorTransformProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the refactor transform provider. Called once at startup by the
/// refactor layer.
pub fn register_refactor_transform_provider(provider: Box<dyn RefactorTransformProvider>) {
    let mut slot = provider_slot()
        .lock()
        .expect("refactor transform provider lock");
    *slot = Some(provider);
}

/// Apply a transform set via the registered provider (or the no-op when the
/// refactor layer is absent).
pub(crate) fn apply_transform_set(
    root: &Path,
    name: &str,
    set: &TransformSet,
    write: bool,
    rerun_hint: Option<String>,
    extra_hints: Vec<String>,
) -> Result<AppliedTransformSummary> {
    let slot = provider_slot()
        .lock()
        .expect("refactor transform provider lock");
    match slot.as_deref() {
        Some(provider) => {
            provider.apply_transform_set(root, name, set, write, rerun_hint, extra_hints)
        }
        None => NoopProvider.apply_transform_set(root, name, set, write, rerun_hint, extra_hints),
    }
}
