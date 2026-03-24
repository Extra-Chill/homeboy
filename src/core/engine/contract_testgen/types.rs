//! types — extracted from contract_testgen.rs.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use super::super::contract::*;
use super::super::*;


/// A plan for generating tests for a single function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestPlan {
    /// The function being tested.
    pub function_name: String,
    /// Source file containing the function.
    pub source_file: String,
    /// Whether the function is async.
    pub is_async: bool,
    /// Individual test cases to generate.
    pub cases: Vec<TestCase>,
}

/// A single test case to generate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    /// Suggested test function name (e.g., "test_validate_write_skips_when_empty").
    pub test_name: String,
    /// Which branch this test covers.
    pub branch_condition: String,
    /// The expected return variant (ok, err, some, none, true, false, value).
    pub expected_variant: String,
    /// Description of what the expected return value should be.
    pub expected_value: Option<String>,
    /// The template key to use for rendering (e.g., "result_ok", "option_none", "bool_true").
    pub template_key: String,
    /// Template variables for rendering.
    pub variables: HashMap<String, String>,
}

/// Overridden setup derived from a branch condition.
pub(crate) struct SetupOverride {
    /// Newline-separated `let` bindings (8-space indented).
    setup_lines: String,
    /// Comma-separated call arguments.
    call_args: String,
    /// Extra `use` imports needed.
    extra_imports: String,
}
/// Generated test output with source code and metadata.
pub struct GeneratedTestOutput {
    /// The rendered test source code (test functions only, no module wrapper).
    pub test_source: String,
    /// Extra `use` imports needed by the generated default values.
    pub extra_imports: Vec<String>,
    /// The function names that tests were generated for.
    pub tested_functions: Vec<String>,
}
