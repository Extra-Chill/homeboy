//! Test failure analysis — cluster failures by root cause.
//!
//! Parses structured test failure data (from extension scripts) and groups
//! failures by similarity. Helps developers prioritize fixes by showing
//! which root causes affect the most tests.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Input: parsed test failures from extension
// ============================================================================

/// A single test failure parsed from test runner output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    /// Fully qualified test name (e.g., "Namespace\\ClassTest::testMethod").
    pub test_name: String,
    /// Test file path relative to component root.
    pub test_file: String,
    /// Error/failure type (e.g., "Error", "PHPUnit\\Framework\\AssertionFailedError").
    pub error_type: String,
    /// Error message.
    pub message: String,
    /// Optional: the source file in the stack trace (deepest non-test frame).
    #[serde(default)]
    pub source_file: String,
    /// Optional: source line number.
    #[serde(default)]
    pub source_line: u32,
}

/// Full test analysis input from extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestAnalysisInput {
    /// All test failures.
    pub failures: Vec<TestFailure>,
    /// Total tests run.
    #[serde(default)]
    pub total: u64,
    /// Total passed.
    #[serde(default)]
    pub passed: u64,
}

// ============================================================================
// Output: clustered analysis
// ============================================================================

/// A cluster of test failures sharing a common root cause.
#[derive(Debug, Clone, Serialize)]
pub struct FailureCluster {
    /// Cluster identifier (derived from the pattern).
    pub id: String,
    /// Human-readable pattern description.
    pub pattern: String,
    /// Category of the failure pattern.
    pub category: FailureCategory,
    /// Number of failures in this cluster.
    pub count: usize,
    /// Test files affected.
    pub affected_files: Vec<String>,
    /// Representative test names (first few).
    pub example_tests: Vec<String>,
    /// Suggested fix if pattern is recognized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

/// Category of a failure cluster.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    /// Method/function doesn't exist.
    MissingMethod,
    /// Class not found.
    MissingClass,
    /// Wrong return type (expected X, got Y).
    ReturnTypeChange,
    /// Wrong error code or message.
    ErrorCodeChange,
    /// Assertion mismatch (expected vs actual).
    AssertionMismatch,
    /// Mock/stub configuration error.
    MockError,
    /// Fatal error (crash, redeclare, etc.).
    FatalError,
    /// Argument count or type mismatch.
    SignatureChange,
    /// Database or environment issue.
    EnvironmentError,
    /// Uncategorized failure.
    Other,
}

/// Full analysis output.
#[derive(Debug, Clone, Serialize)]
pub struct TestAnalysis {
    /// Component that was analyzed.
    pub component: String,
    /// Total test failures.
    pub total_failures: usize,
    /// Total tests run.
    pub total_tests: u64,
    /// Total passing.
    pub total_passed: u64,
    /// Failure clusters, sorted by count (largest first).
    pub clusters: Vec<FailureCluster>,
    /// Human-readable hints.
    pub hints: Vec<String>,
}

// ============================================================================
// Clustering engine
// ============================================================================

/// Analyze test failures and cluster them by root cause.
pub fn analyze(component: &str, input: &TestAnalysisInput) -> TestAnalysis {
    let failures = &input.failures;

    if failures.is_empty() {
        return TestAnalysis {
            component: component.to_string(),
            total_failures: 0,
            total_tests: input.total,
            total_passed: input.passed,
            clusters: Vec::new(),
            hints: vec!["All tests passing — nothing to analyze.".to_string()],
        };
    }

    // Step 1: Generate a cluster key for each failure
    let mut cluster_map: HashMap<String, Vec<&TestFailure>> = HashMap::new();
    for failure in failures {
        let key = cluster_key(failure);
        cluster_map.entry(key).or_default().push(failure);
    }

    // Step 2: Build cluster objects
    let mut clusters: Vec<FailureCluster> = cluster_map
        .into_iter()
        .map(|(key, members)| {
            let category = categorize(&members[0].error_type, &members[0].message);
            let pattern = derive_pattern(&members);
            let suggested_fix = suggest_fix(&category, &members[0].message);

            let mut affected_files: Vec<String> =
                members.iter().map(|f| f.test_file.clone()).collect();
            affected_files.sort();
            affected_files.dedup();

            let example_tests: Vec<String> = members
                .iter()
                .take(5)
                .map(|f| f.test_name.clone())
                .collect();

            FailureCluster {
                id: key,
                pattern,
                category,
                count: members.len(),
                affected_files,
                example_tests,
                suggested_fix,
            }
        })
        .collect();

    // Step 3: Sort by count descending
    clusters.sort_by(|a, b| b.count.cmp(&a.count));

    // Step 4: Generate hints
    let hints = generate_hints(&clusters, failures.len());

    TestAnalysis {
        component: component.to_string(),
        total_failures: failures.len(),
        total_tests: input.total,
        total_passed: input.passed,
        clusters,
        hints,
    }
}

/// Generate a stable cluster key from a failure.
///
/// The key should group failures that have the same root cause.
/// We normalize the message to remove test-specific details.
fn cluster_key(failure: &TestFailure) -> String {
    let msg = &failure.message;

    // Pattern: "Call to undefined method ClassName::methodName()"
    if let Some(pattern) = extract_pattern(msg, "Call to undefined method ", "(") {
        return format!("missing_method::{}", pattern);
    }

    // Pattern: "Class \"ClassName\" not found"
    if let Some(pattern) = extract_pattern(msg, "Class \"", "\" not found") {
        return format!("missing_class::{}", pattern);
    }

    // Pattern: "Call to undefined function functionName()"
    if let Some(pattern) = extract_pattern(msg, "Call to undefined function ", "(") {
        return format!("missing_function::{}", pattern);
    }

    // Pattern: "Cannot redeclare functionName()"
    if msg.contains("Cannot redeclare") {
        let fn_name = extract_between(msg, "Cannot redeclare ", "(")
            .unwrap_or_else(|| extract_between(msg, "Cannot redeclare ", " ").unwrap_or("unknown"));
        return format!("fatal_redeclare::{}", fn_name);
    }

    // Pattern: "Failed asserting that X is an instance of Y"
    if msg.contains("is an instance of") {
        if let Some(expected) = extract_between(msg, "instance of \"", "\"") {
            return format!("wrong_type::{}", expected);
        }
    }

    // Pattern: "Failed asserting that X matches expected Y"
    // or "Failed asserting that 'actual' is identical to 'expected'"
    if msg.contains("Failed asserting") {
        // Try to extract the assertion type
        if msg.contains("is identical to") {
            return format!("assertion_mismatch::identical::{}", normalize_for_key(msg));
        }
        if msg.contains("matches expected") {
            return format!("assertion_mismatch::expected::{}", normalize_for_key(msg));
        }
        if msg.contains("is true") || msg.contains("is false") {
            return format!("assertion_mismatch::boolean::{}", normalize_for_key(msg));
        }
        if msg.contains("null") {
            return format!("assertion_mismatch::null::{}", normalize_for_key(msg));
        }
    }

    // Pattern: WP_Error with specific error code
    if msg.contains("WP_Error") || msg.contains("wp_error") {
        if let Some(code) = extract_between(msg, "code: ", ")") {
            return format!("wp_error::{}", code);
        }
        if let Some(code) = extract_between(msg, "'", "'") {
            return format!("wp_error::{}", code);
        }
    }

    // Pattern: "Argument #N ... must be of type X, Y given"
    if msg.contains("must be of type") {
        let key = normalize_for_key(msg);
        return format!("type_error::{}", key);
    }

    // Pattern: Mock/stub errors
    if msg.contains("MockObject") || msg.contains("mock") || msg.contains("stub") {
        return format!("mock_error::{}", normalize_for_key(msg));
    }

    // Pattern: PHPUnit configuration errors
    if msg.contains("configure()") || msg.contains("getMock") {
        return format!("mock_config::{}", normalize_for_key(msg));
    }

    // Fallback: hash the error type + normalized message
    format!(
        "{}::{}",
        failure.error_type.replace('\\', "_"),
        normalize_for_key(msg)
    )
}

/// Categorize a failure by error type and message.
fn categorize(error_type: &str, message: &str) -> FailureCategory {
    // Fatal errors
    if message.contains("Cannot redeclare")
        || message.contains("Fatal error")
        || error_type.contains("Fatal")
    {
        return FailureCategory::FatalError;
    }

    // Missing methods/functions
    if message.contains("Call to undefined method")
        || message.contains("Call to undefined function")
    {
        return FailureCategory::MissingMethod;
    }

    // Missing classes
    if message.contains("not found") && message.contains("Class") {
        return FailureCategory::MissingClass;
    }

    // Type/signature issues
    if message.contains("must be of type")
        || message.contains("Argument #")
        || message.contains("Too few arguments")
    {
        return FailureCategory::SignatureChange;
    }

    // Return type changes
    if message.contains("is an instance of") || message.contains("Return value must be") {
        return FailureCategory::ReturnTypeChange;
    }

    // Error code changes (WP_Error, HTTP status, etc.)
    if message.contains("error code")
        || message.contains("WP_Error")
        || message.contains("rest_forbidden")
        || message.contains("ability_invalid")
    {
        return FailureCategory::ErrorCodeChange;
    }

    // Mock errors
    if message.contains("MockObject")
        || message.contains("configure()")
        || message.contains("cannot be configured")
        || message.contains("getMock")
        || error_type.contains("Mock")
    {
        return FailureCategory::MockError;
    }

    // Database/environment
    if message.contains("SQLITE")
        || message.contains("MySQL")
        || message.contains("table")
        || message.contains("database")
    {
        return FailureCategory::EnvironmentError;
    }

    // Assertion mismatches (generic)
    if message.contains("Failed asserting") {
        return FailureCategory::AssertionMismatch;
    }

    FailureCategory::Other
}

/// Derive a human-readable pattern from a cluster's members.
fn derive_pattern(members: &[&TestFailure]) -> String {
    // If all share the same message, use it directly
    let first_msg = &members[0].message;
    if members.iter().all(|f| f.message == *first_msg) {
        return truncate(first_msg, 120);
    }

    // Find common prefix
    let messages: Vec<&str> = members.iter().map(|f| f.message.as_str()).collect();
    let common = common_prefix(&messages);
    if common.len() > 20 {
        return format!("{}... ({} variants)", truncate(&common, 80), members.len());
    }

    // Fall back to the first member's message
    truncate(first_msg, 120)
}

/// Suggest a fix for recognized failure patterns.
fn suggest_fix(category: &FailureCategory, message: &str) -> Option<String> {
    match category {
        FailureCategory::MissingMethod => {
            if let Some(method) = extract_between(message, "::", "(") {
                Some(format!(
                    "Method '{}' was removed or renamed — check production code for the new name",
                    method
                ))
            } else {
                Some("Method was removed or renamed — check production code".to_string())
            }
        }
        FailureCategory::MissingClass => {
            Some("Class was moved or renamed — update imports and references".to_string())
        }
        FailureCategory::FatalError => {
            if message.contains("Cannot redeclare") {
                Some("Function is being included twice — check bootstrap and autoloading".to_string())
            } else {
                Some("Fatal error in test bootstrap — fix before other tests can run".to_string())
            }
        }
        FailureCategory::ErrorCodeChange => {
            Some("Error codes changed — update assertion strings to match new API".to_string())
        }
        FailureCategory::ReturnTypeChange => {
            Some("Return type changed — update assertions (e.g., assertIsArray → assertInstanceOf(WP_Error::class))".to_string())
        }
        FailureCategory::MockError => {
            Some("Mock configuration broken — the mocked class/method signature changed".to_string())
        }
        FailureCategory::SignatureChange => {
            Some("Method signature changed — update call sites with new parameter list".to_string())
        }
        _ => None,
    }
}

/// Generate human-readable hints from the analysis.
fn generate_hints(clusters: &[FailureCluster], total: usize) -> Vec<String> {
    let mut hints = Vec::new();

    if clusters.is_empty() {
        return hints;
    }

    // Hint: largest cluster
    let largest = &clusters[0];
    hints.push(format!(
        "Largest cluster: {} failure(s) — {}",
        largest.count,
        truncate(&largest.pattern, 80),
    ));

    // Hint: top 3 cover what percentage
    let top3_count: usize = clusters.iter().take(3).map(|c| c.count).sum();
    if clusters.len() > 1 {
        hints.push(format!(
            "Top {} cluster(s) account for {}/{} failures ({:.0}%)",
            clusters.len().min(3),
            top3_count,
            total,
            (top3_count as f64 / total as f64) * 100.0,
        ));
    }

    // Hint: fatal errors should be fixed first
    let fatal_count: usize = clusters
        .iter()
        .filter(|c| c.category == FailureCategory::FatalError)
        .map(|c| c.count)
        .sum();
    if fatal_count > 0 {
        hints.push(format!(
            "Fix fatal errors first — {} failure(s) may be blocking other tests",
            fatal_count,
        ));
    }

    // Hint: auto-fixable patterns
    let auto_fixable: usize = clusters
        .iter()
        .filter(|c| {
            matches!(
                c.category,
                FailureCategory::ErrorCodeChange
                    | FailureCategory::MissingMethod
                    | FailureCategory::MissingClass
            )
        })
        .map(|c| c.count)
        .sum();
    if auto_fixable > 0 {
        hints.push(format!(
            "{} failure(s) are likely fixable with find-replace (renamed methods, changed error codes)",
            auto_fixable,
        ));
    }

    hints
}

// ============================================================================
// String helpers
// ============================================================================

/// Extract a substring between two delimiters.
fn extract_between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = s.find(start)?;
    let after_start = start_idx + start.len();
    let end_idx = s[after_start..].find(end)?;
    Some(&s[after_start..after_start + end_idx])
}

/// Extract a pattern: text after `prefix` up to `end_marker`.
fn extract_pattern<'a>(s: &'a str, prefix: &str, end_marker: &str) -> Option<&'a str> {
    if !s.contains(prefix) {
        return None;
    }
    extract_between(s, prefix, end_marker)
}

/// Normalize a message into a stable key by removing variable parts.
fn normalize_for_key(msg: &str) -> String {
    // Remove quoted strings (file paths, class names vary)
    let mut result = msg.to_string();

    // Collapse whitespace
    result = result.split_whitespace().collect::<Vec<_>>().join(" ");

    // Truncate for key stability
    if result.len() > 80 {
        result.truncate(80);
    }

    // Replace characters that are bad for keys
    result
        .replace(['/', '\\', '"', '\'', ':', '.'], "_")
        .replace(' ', "_")
        .to_lowercase()
}

/// Find the longest common prefix of a set of strings.
fn common_prefix(strings: &[&str]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let first = strings[0];
    let mut prefix_len = first.len();

    for s in &strings[1..] {
        prefix_len = prefix_len.min(s.len());
        for (i, (a, b)) in first.bytes().zip(s.bytes()).enumerate() {
            if a != b {
                prefix_len = prefix_len.min(i);
                break;
            }
        }
    }

    first[..prefix_len].to_string()
}

/// Truncate a string to max length, adding "..." if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.min(s.len())])
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(test: &str, file: &str, error_type: &str, message: &str) -> TestFailure {
        TestFailure {
            test_name: test.to_string(),
            test_file: file.to_string(),
            error_type: error_type.to_string(),
            message: message.to_string(),
            source_file: String::new(),
            source_line: 0,
        }
    }

    fn input(failures: Vec<TestFailure>) -> TestAnalysisInput {
        let total = failures.len() as u64 + 10; // assume some passed
        let passed = 10;
        TestAnalysisInput {
            failures,
            total,
            passed,
        }
    }

    #[test]
    fn empty_failures_produces_no_clusters() {
        let result = analyze("test-component", &input(vec![]));
        assert_eq!(result.total_failures, 0);
        assert!(result.clusters.is_empty());
    }

    #[test]
    fn identical_messages_cluster_together() {
        let failures = vec![
            failure(
                "FooTest::testA",
                "tests/FooTest.php",
                "Error",
                "Call to undefined method PluginSettings::set()",
            ),
            failure(
                "BarTest::testB",
                "tests/BarTest.php",
                "Error",
                "Call to undefined method PluginSettings::set()",
            ),
            failure(
                "BazTest::testC",
                "tests/BazTest.php",
                "Error",
                "Call to undefined method PluginSettings::set()",
            ),
        ];

        let result = analyze("test", &input(failures));
        assert_eq!(result.clusters.len(), 1);
        assert_eq!(result.clusters[0].count, 3);
        assert_eq!(result.clusters[0].category, FailureCategory::MissingMethod);
    }

    #[test]
    fn different_undefined_methods_get_separate_clusters() {
        let failures = vec![
            failure(
                "FooTest::testA",
                "tests/FooTest.php",
                "Error",
                "Call to undefined method PluginSettings::set()",
            ),
            failure(
                "BarTest::testB",
                "tests/BarTest.php",
                "Error",
                "Call to undefined method PluginSettings::delete()",
            ),
        ];

        let result = analyze("test", &input(failures));
        assert_eq!(result.clusters.len(), 2);
        assert_eq!(result.clusters[0].count, 1);
        assert_eq!(result.clusters[1].count, 1);
    }

    #[test]
    fn fatal_redeclare_clusters() {
        let failures = vec![
            failure(
                "FooTest::testA",
                "tests/FooTest.php",
                "Fatal",
                "Cannot redeclare datamachine_get_monolog_instance()",
            ),
            failure(
                "BarTest::testB",
                "tests/BarTest.php",
                "Fatal",
                "Cannot redeclare datamachine_get_monolog_instance()",
            ),
        ];

        let result = analyze("test", &input(failures));
        assert_eq!(result.clusters.len(), 1);
        assert_eq!(result.clusters[0].category, FailureCategory::FatalError);
        assert!(result.clusters[0].suggested_fix.is_some());
    }

    #[test]
    fn sorted_by_count_descending() {
        let failures = vec![
            failure(
                "A::a",
                "a.php",
                "Error",
                "Call to undefined method X::foo()",
            ),
            failure("B::b", "b.php", "Error", "Class \"Missing\" not found"),
            failure("C::c", "c.php", "Error", "Class \"Missing\" not found"),
            failure("D::d", "d.php", "Error", "Class \"Missing\" not found"),
        ];

        let result = analyze("test", &input(failures));
        assert_eq!(result.clusters[0].count, 3); // missing class
        assert_eq!(result.clusters[1].count, 1); // undefined method
    }

    #[test]
    fn mock_errors_categorized() {
        let failures = vec![failure(
            "FooTest::testA",
            "tests/FooTest.php",
            "Error",
            "Trying to configure method \"execute\" which cannot be configured because it does not exist, has not been specified, is final, or is static",
        )];

        let result = analyze("test", &input(failures));
        assert_eq!(result.clusters[0].category, FailureCategory::MockError);
    }

    #[test]
    fn return_type_change_detected() {
        let failures = vec![failure(
            "FooTest::testA",
            "tests/FooTest.php",
            "AssertionFailedError",
            "Failed asserting that WP_Error Object (...) is an instance of \"array\"",
        )];

        let result = analyze("test", &input(failures));
        assert_eq!(
            result.clusters[0].category,
            FailureCategory::ReturnTypeChange
        );
    }

    #[test]
    fn hints_include_fix_priority() {
        let failures = vec![
            failure("A::a", "a.php", "Fatal", "Cannot redeclare foo()"),
            failure("B::b", "b.php", "Fatal", "Cannot redeclare foo()"),
            failure("C::c", "c.php", "Fatal", "Cannot redeclare foo()"),
            failure(
                "D::d",
                "d.php",
                "Error",
                "Call to undefined method X::bar()",
            ),
        ];

        let result = analyze("test", &input(failures));
        let hints_text = result.hints.join(" ");
        assert!(hints_text.contains("fatal"));
    }

    #[test]
    fn affected_files_deduplicated() {
        let failures = vec![
            failure(
                "FooTest::testA",
                "tests/FooTest.php",
                "Error",
                "Call to undefined method X::foo()",
            ),
            failure(
                "FooTest::testB",
                "tests/FooTest.php",
                "Error",
                "Call to undefined method X::foo()",
            ),
        ];

        let result = analyze("test", &input(failures));
        assert_eq!(result.clusters[0].affected_files.len(), 1);
        assert_eq!(result.clusters[0].count, 2);
    }

    #[test]
    fn extract_between_works() {
        assert_eq!(
            extract_between("Class \"Foo\\Bar\" not found", "Class \"", "\" not found"),
            Some("Foo\\Bar")
        );
        assert_eq!(extract_between("no match here", "start", "end"), None);
    }

    #[test]
    fn common_prefix_works() {
        assert_eq!(common_prefix(&["foobar", "foobaz", "fooqux"]), "foo");
        assert_eq!(common_prefix(&["abc"]), "abc");
        assert_eq!(common_prefix(&[]), "");
    }

    #[test]
    fn signature_change_categorized() {
        let failures = vec![failure(
            "FooTest::testA",
            "tests/FooTest.php",
            "TypeError",
            "Too few arguments to function Foo::bar(), 2 passed and exactly 3 expected",
        )];

        let result = analyze("test", &input(failures));
        assert_eq!(
            result.clusters[0].category,
            FailureCategory::SignatureChange
        );
    }
}
