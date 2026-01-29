//! Input validation primitives.
//!
//! Provides ergonomic helpers for common validation patterns:
//! - Unwrapping Option values with descriptive errors
//! - Validating non-empty strings and collections
//!
//! These replace verbose ok_or_else + Error::validation_invalid_argument chains.

use crate::error::{Error, Result};

/// Require an Option to contain a value.
///
/// Replaces the common pattern:
/// ```ignore
/// value.ok_or_else(|| Error::validation_invalid_argument("field", "msg", None, None))?
/// ```
///
/// With:
/// ```ignore
/// validation::require(value, "field", "msg")?
/// ```
pub fn require<T>(opt: Option<T>, field: &str, message: &str) -> Result<T> {
    opt.ok_or_else(|| Error::validation_invalid_argument(field, message, None, None))
}

/// Require an Option to contain a value, with hints for resolution.
pub fn require_with_hints<T>(
    opt: Option<T>,
    field: &str,
    message: &str,
    hints: Vec<String>,
) -> Result<T> {
    opt.ok_or_else(|| Error::validation_invalid_argument(field, message, None, Some(hints)))
}

/// Require a string to be non-empty after trimming.
///
/// Returns a reference to the trimmed string on success.
pub fn require_non_empty<'a>(value: &'a str, field: &str, message: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(Error::validation_invalid_argument(field, message, None, None))
    } else {
        Ok(trimmed)
    }
}

/// Require a collection to be non-empty.
pub fn require_non_empty_vec<'a, T>(vec: &'a [T], field: &str, message: &str) -> Result<&'a [T]> {
    if vec.is_empty() {
        Err(Error::validation_invalid_argument(field, message, None, None))
    } else {
        Ok(vec)
    }
}

/// Collects validation errors for aggregated reporting.
///
/// Use when a command has multiple independent validations that should
/// all be checked before returning, rather than failing on the first error.
///
/// # Example
/// ```ignore
/// let mut v = ValidationCollector::new();
///
/// // Capture errors from Result-returning functions
/// let version = v.capture(read_version(id), "version");
/// let changelog = v.capture(validate_changelog(&comp), "changelog");
///
/// // Add custom errors
/// if uncommitted_files.len() > 0 {
///     v.push("working_tree", "Uncommitted changes detected", Some(json!({"files": files})));
/// }
///
/// // Return all errors at once (or Ok if none)
/// v.finish()?;
/// ```
pub struct ValidationCollector {
    errors: Vec<crate::error::ValidationErrorItem>,
}

impl ValidationCollector {
    pub fn new() -> Self {
        Self { errors: Vec::new() }
    }

    /// Capture a Result, storing the error if Err and returning the Ok value.
    pub fn capture<T>(&mut self, result: Result<T>, field: &str) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(err) => {
                self.errors.push(crate::error::ValidationErrorItem {
                    field: field.to_string(),
                    problem: err.message.clone(),
                    context: if err.details.is_object()
                        && !err.details.as_object().unwrap().is_empty()
                    {
                        Some(err.details)
                    } else {
                        None
                    },
                });
                None
            }
        }
    }

    /// Add an error directly.
    pub fn push(&mut self, field: &str, problem: &str, context: Option<serde_json::Value>) {
        self.errors.push(crate::error::ValidationErrorItem {
            field: field.to_string(),
            problem: problem.to_string(),
            context,
        });
    }

    /// Check if any errors have been collected.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Consume the collector and return Result.
    /// - No errors: Ok(())
    /// - Single error: Err(ValidationInvalidArgument) for backward compat
    /// - Multiple errors: Err(ValidationMultipleErrors)
    pub fn finish(self) -> Result<()> {
        match self.errors.len() {
            0 => Ok(()),
            1 => {
                let err = &self.errors[0];
                Err(Error::validation_invalid_argument(
                    &err.field, &err.problem, None, None,
                ))
            }
            _ => Err(Error::validation_multiple_errors(self.errors)),
        }
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

    #[test]
    fn require_returns_value_when_some() {
        let result = require(Some("value"), "field", "msg");
        assert_eq!(result.unwrap(), "value");
    }

    #[test]
    fn require_returns_error_when_none() {
        let result: Result<&str> = require(None, "field", "Missing field");
        assert!(result.is_err());
    }

    #[test]
    fn require_non_empty_passes_for_non_empty() {
        let result = require_non_empty("hello", "field", "msg");
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn require_non_empty_trims_whitespace() {
        let result = require_non_empty("  hello  ", "field", "msg");
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn require_non_empty_fails_for_empty() {
        let result = require_non_empty("", "field", "Cannot be empty");
        assert!(result.is_err());
    }

    #[test]
    fn require_non_empty_fails_for_whitespace_only() {
        let result = require_non_empty("   ", "field", "Cannot be empty");
        assert!(result.is_err());
    }

    #[test]
    fn require_non_empty_vec_passes_for_non_empty() {
        let vec = vec![1, 2, 3];
        let result = require_non_empty_vec(&vec, "field", "msg");
        assert_eq!(result.unwrap(), &[1, 2, 3]);
    }

    #[test]
    fn require_non_empty_vec_fails_for_empty() {
        let vec: Vec<i32> = vec![];
        let result = require_non_empty_vec(&vec, "field", "Cannot be empty");
        assert!(result.is_err());
    }

    #[test]
    fn collector_finish_returns_ok_when_no_errors() {
        let v = ValidationCollector::new();
        assert!(v.finish().is_ok());
    }

    #[test]
    fn collector_finish_returns_single_error() {
        use crate::error::ErrorCode;

        let mut v = ValidationCollector::new();
        v.push("field1", "Problem 1", None);
        let err = v.finish().unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    }

    #[test]
    fn collector_finish_returns_multiple_errors() {
        use crate::error::ErrorCode;

        let mut v = ValidationCollector::new();
        v.push("field1", "Problem 1", None);
        v.push("field2", "Problem 2", None);
        let err = v.finish().unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationMultipleErrors);
        assert!(err.message.contains("2 validation issue"));
    }

    #[test]
    fn collector_capture_stores_error_and_returns_none() {
        let mut v = ValidationCollector::new();
        let result: Option<i32> = v.capture(
            Err(Error::validation_invalid_argument("test", "msg", None, None)),
            "test",
        );
        assert!(result.is_none());
        assert!(v.has_errors());
    }

    #[test]
    fn collector_capture_returns_value_on_success() {
        let mut v = ValidationCollector::new();
        let result: Option<i32> = v.capture(Ok(42), "test");
        assert_eq!(result, Some(42));
        assert!(!v.has_errors());
    }
}
