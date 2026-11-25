//! Input validation primitives.

use homeboy_error::{Error, Result};

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
    errors: Vec<homeboy_error::ValidationErrorItem>,
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

                self.errors.push(homeboy_error::ValidationErrorItem {
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
        self.errors.push(homeboy_error::ValidationErrorItem {
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
fn single_error_to_invalid_argument(err: &homeboy_error::ValidationErrorItem) -> Error {
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
    use serde_json::json;

    /// `finish` and `finish_if_errors` must return identical shapes so CLI
    /// envelopes treat early-exit and end-of-pipeline failures alike.
    #[test]
    fn both_finish_paths_return_the_same_error_shapes() {
        let finishes: [fn(ValidationCollector) -> Result<()>; 2] = [
            |collector| collector.finish(),
            |mut collector| collector.finish_if_errors(),
        ];
        for finish in finishes {
            assert!(finish(ValidationCollector::new()).is_ok());

            // A single error without context keeps the field/problem shape.
            let mut v = ValidationCollector::new();
            v.push("field", "problem", None);
            let err = finish(v).unwrap_err();
            assert_eq!(err.details["field"], "field");
            assert_eq!(err.details["problem"], "problem");

            // A single error's structured context (e.g. release dirty-file
            // lists) is preserved verbatim for JSON consumers.
            let context = json!({ "files": ["target/foo.rs", "Cargo.lock"] });
            let mut v = ValidationCollector::new();
            v.push("working_tree", "Uncommitted changes", Some(context.clone()));
            assert_eq!(finish(v).unwrap_err().details, context);

            // Multiple errors surface as one structured validation error.
            let mut v = ValidationCollector::new();
            v.push("a", "first", None);
            v.push("b", "second", None);
            let err = finish(v).unwrap_err();
            assert!(err.message.to_lowercase().contains("validation"));
        }
    }

    #[test]
    fn finish_if_errors_drains_the_collector_for_reuse() {
        let mut v = ValidationCollector::new();
        assert!(!v.has_errors());
        v.push("field_a", "problem a", None);
        assert!(v.has_errors());
        assert!(v.finish_if_errors().is_err());
        assert!(!v.has_errors());
        assert!(v.finish_if_errors().is_ok());
    }
}
