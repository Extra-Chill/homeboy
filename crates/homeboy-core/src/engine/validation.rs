//! Input validation primitives.

use crate::error::{Error, Result};

pub fn require<T>(opt: Option<T>, field: &str, message: &str) -> Result<T> {
    opt.ok_or_else(|| Error::validation_invalid_argument(field, message, None, None))
}

pub fn require_with_hints<T>(
    opt: Option<T>,
    field: &str,
    message: &str,
    hints: Vec<String>,
) -> Result<T> {
    opt.ok_or_else(|| Error::validation_invalid_argument(field, message, None, Some(hints)))
}

pub struct ValidationCollector {
    errors: Vec<crate::error::ValidationErrorItem>,
}

impl ValidationCollector {
    pub fn new() -> Self {
        Self { errors: Vec::new() }
    }

    pub fn capture<T>(&mut self, result: Result<T>, field: &str) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(err) => {
                let problem = err
                    .details
                    .get("problem")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| err.message.clone());

                self.errors.push(crate::error::ValidationErrorItem {
                    field: field.to_string(),
                    problem,
                    context: if err.details.as_object().is_some_and(|o| !o.is_empty()) {
                        Some(err.details)
                    } else {
                        None
                    },
                });
                None
            }
        }
    }

    pub fn push(&mut self, field: &str, problem: &str, context: Option<serde_json::Value>) {
        self.errors.push(crate::error::ValidationErrorItem {
            field: field.to_string(),
            problem: problem.to_string(),
            context,
        });
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn finish(self) -> Result<()> {
        match self.errors.len() {
            0 => Ok(()),
            1 => Err(single_error_to_invalid_argument(&self.errors[0])),
            _ => Err(Error::validation_multiple_errors(self.errors)),
        }
    }

    /// Bail out immediately if any errors have been collected so far.
    ///
    /// Use this between validation stages to fail fast before running expensive
    /// or output-heavy checks (lint, test, builds) that would drown out the
    /// real reason the operation was blocked.
    ///
    /// On error, drains the collector and returns the same shape that
    /// [`Self::finish`] would have returned — so existing error-handling code
    /// (CLI envelopes, JSON output, exit codes) treats early-exit and
    /// end-of-pipeline failures identically.
    pub fn finish_if_errors(&mut self) -> Result<()> {
        if self.errors.is_empty() {
            return Ok(());
        }
        let drained: Vec<_> = std::mem::take(&mut self.errors);
        match drained.len() {
            1 => Err(single_error_to_invalid_argument(&drained[0])),
            _ => Err(Error::validation_multiple_errors(drained)),
        }
    }
}

/// Convert a single collected validation error back into an [`Error`].
///
/// Preserves any structured `context` payload the original validator attached
/// (file lists, hints, deeper diagnostic data). Without this, the single-error
/// re-emit path would drop everything except `field`/`problem` — making it
/// impossible to debug failures that bottle up via `ValidationCollector`
/// (e.g. release working-tree checks emitting the dirty file list).
fn single_error_to_invalid_argument(err: &crate::error::ValidationErrorItem) -> Error {
    let base = Error::validation_invalid_argument(&err.field, &err.problem, None, None);
    match &err.context {
        Some(context) => Error {
            details: context.clone(),
            ..base
        },
        None => base,
    }
}

impl Default for ValidationCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Audit's `MissingTestMethod` rule uses an exact-name match: stripping
    // the `test_` prefix from a test must produce the source method name
    // verbatim. So there is one `test_<method>` per public method, and
    // additional scenario tests use a `_` suffix so they read naturally
    // without the auditor treating them as coverage.

    #[test]
    fn test_push() {
        let mut v = ValidationCollector::new();
        assert!(!v.has_errors());
        v.push("field", "problem", None);
        assert!(v.has_errors());
    }

    #[test]
    fn test_finish() {
        // Clean collector → Ok.
        let v = ValidationCollector::new();
        assert!(v.finish().is_ok());
    }

    #[test]
    fn test_finish_single_error_shape() {
        let mut v = ValidationCollector::new();
        v.push("only", "one problem", None);
        let err = v.finish().unwrap_err();
        assert!(err.message.contains("one problem"));
    }

    #[test]
    fn test_finish_multiple_errors_shape() {
        let mut v = ValidationCollector::new();
        v.push("a", "first", None);
        v.push("b", "second", None);
        let err = v.finish().unwrap_err();
        assert!(err.message.to_lowercase().contains("validation"));
    }

    #[test]
    fn test_finish_if_errors() {
        // Clean collector → Ok, and the collector stays reusable.
        let mut v = ValidationCollector::new();
        assert!(v.finish_if_errors().is_ok());
        v.push("field", "problem", None);
        assert!(v.finish_if_errors().is_err());
    }

    #[test]
    fn test_finish_if_errors_drains_collector() {
        let mut v = ValidationCollector::new();
        v.push("field_a", "problem a", None);
        assert!(v.finish_if_errors().is_err());
        // After draining, the collector is empty again.
        assert!(!v.has_errors());
        assert!(v.finish_if_errors().is_ok());
    }

    #[test]
    fn test_finish_if_errors_multiple_errors_shape() {
        let mut v = ValidationCollector::new();
        v.push("a", "first", None);
        v.push("b", "second", None);
        let err = v.finish_if_errors().unwrap_err();
        // Multiple errors flow through validation_multiple_errors and
        // surface as a structured payload (not a single invalid_argument).
        assert!(err.message.to_lowercase().contains("validation"));
    }

    #[test]
    fn test_finish_if_errors_preserves_single_error_context() {
        // Regression: the single-error re-emit path used to drop the
        // `context` JSON entirely, making structured diagnostics
        // (e.g. release working-tree dirty file lists) invisible to
        // JSON consumers. Now context is preserved verbatim.
        let mut v = ValidationCollector::new();
        let context = serde_json::json!({
            "files": ["target/foo.rs", "Cargo.lock"],
            "hint": "Commit, stash, or discard changes before releasing",
        });
        v.push(
            "working_tree",
            "Uncommitted changes detected",
            Some(context.clone()),
        );

        let err = v.finish_if_errors().unwrap_err();
        assert_eq!(err.details, context);
    }

    #[test]
    fn test_finish_preserves_single_error_context() {
        // Same regression as above, but on the consume-self `finish` path
        // taken by validators that don't need fail-fast semantics.
        let mut v = ValidationCollector::new();
        let context = serde_json::json!({"files": ["only.rs"]});
        v.push("field", "problem", Some(context.clone()));

        let err = v.finish().unwrap_err();
        assert_eq!(err.details, context);
    }

    #[test]
    fn test_finish_if_errors_single_error_without_context_works() {
        // Errors collected without context still serialize cleanly — the
        // preservation logic must not require context to be present.
        let mut v = ValidationCollector::new();
        v.push("field", "problem", None);
        let err = v.finish_if_errors().unwrap_err();
        // Without context, the base validation_invalid_argument shape
        // (field + problem) is what we get.
        assert_eq!(
            err.details.get("field").and_then(|v| v.as_str()),
            Some("field")
        );
        assert_eq!(
            err.details.get("problem").and_then(|v| v.as_str()),
            Some("problem")
        );
    }
}
