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
            1 => {
                let err = &self.errors[0];
                Err(Error::validation_invalid_argument(
                    &err.field,
                    &err.problem,
                    None,
                    None,
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
    fn test_new_match_result() {
        let instance = ValidationCollector::default();
        let _result = instance.new();
    }

    #[test]
    fn test_new_match_result() {
        let instance = ValidationCollector::default();
        let _result = instance.new();
    }

    #[test]
    fn test_new_some_err_details() {
        let instance = ValidationCollector::default();
        let _result = instance.new();
    }

    #[test]
    fn test_new_else() {
        let instance = ValidationCollector::default();
        let _result = instance.new();
    }

    #[test]
    fn test_new_else() {
        let instance = ValidationCollector::default();
        let _result = instance.new();
    }

    #[test]
    fn test_new_match_self_errors_len() {
        let instance = ValidationCollector::default();
        let _result = instance.new();
    }

    #[test]
    fn test_new_err_error_validation_multiple_errors_self_errors() {
        let instance = ValidationCollector::default();
        let _result = instance.new();
    }

    #[test]
    fn test_new_has_expected_effects() {
        // Expected effects: mutation
        let instance = ValidationCollector::default();
        let _ = instance.new();
    }

    #[test]
    fn test_push_match_self_errors_len() {
        let mut instance = ValidationCollector::default();
        let field = "";
        let problem = "";
        let context = None;
        let result = instance.push(&field, &problem, context);
        assert!(result.is_ok(), "expected Ok for: match self.errors.len()");
    }

    #[test]
    fn test_push_err_error_validation_multiple_errors_self_errors() {
        let mut instance = ValidationCollector::default();
        let field = "";
        let problem = "";
        let context = None;
        let result = instance.push(&field, &problem, context);
        assert!(
            result.is_ok(),
            "expected Ok for: _ => Err(Error::validation_multiple_errors(self.errors)),"
        );
    }

    #[test]
    fn test_push_has_expected_effects() {
        // Expected effects: mutation
        let mut instance = ValidationCollector::default();
        let field = "";
        let problem = "";
        let context = None;
        let _ = instance.push(&field, &problem, context);
    }

    #[test]
    fn test_has_errors_match_self_errors_len() {
        let instance = ValidationCollector::default();
        let _result = instance.has_errors();
    }

    #[test]
    fn test_has_errors_err_error_validation_multiple_errors_self_errors() {
        let instance = ValidationCollector::default();
        let _result = instance.has_errors();
    }

    #[test]
    fn test_finish_match_self_errors_len() {
        let instance = ValidationCollector::default();
        let result = instance.finish();
        let inner = result.unwrap();
        // Branch returns Ok(() when: match self.errors.len()
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_finish_err_error_validation_multiple_errors_self_errors() {
        let instance = ValidationCollector::default();
        let result = instance.finish();
        let err = result.unwrap_err();
        // Branch returns Err(Error::validation_multiple_errors(self.errors) when: _ => Err(Error::validation_multiple_errors(self.errors)),
        let err_msg = format!("{:?}", err);
        let _ = err_msg; // TODO: assert error contains "Error::validation_multiple_errors(self.errors"
    }
}
