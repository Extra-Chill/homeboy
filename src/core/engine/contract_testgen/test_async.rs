//! test_async — extracted from contract_testgen.rs.

use std::collections::HashMap;
use super::TestPlan;
use super::TestCase;
use super::super::contract::*;
use super::super::*;


/// Render a test plan into source code using templates.
///
/// Templates are key → string pairs where keys match `TestCase.template_key`.
/// Template variables are replaced: `{fn_name}`, `{fn_call}`, `{param_list}`, etc.
pub(crate) fn render_test_plan(plan: &TestPlan, templates: &HashMap<String, String>) -> String {
    let mut output = String::new();
    let mut seen_names: HashMap<String, usize> = HashMap::new();

    for case in &plan.cases {
        let template = match templates.get(&case.template_key) {
            Some(t) => t,
            None => {
                // Fall back to a generic template if the specific one doesn't exist
                match templates.get("default") {
                    Some(t) => t,
                    None => continue,
                }
            }
        };

        // Deduplicate test names by appending a numeric suffix when a name
        // has been seen before. This prevents compilation errors from branches
        // with identical slugified conditions (e.g. two `None => return false`
        // match arms producing the same test name). (#818)
        let unique_name = {
            let count = seen_names.entry(case.test_name.clone()).or_insert(0);
            *count += 1;
            if *count == 1 {
                case.test_name.clone()
            } else {
                format!("{}_{}", case.test_name, count)
            }
        };

        let mut rendered = template.clone();
        for (key, value) in &case.variables {
            rendered = rendered.replace(&format!("{{{}}}", key), value);
        }
        // Also replace the test name
        rendered = rendered.replace("{test_name}", &unique_name);

        // For async functions, transform the test to use #[tokio::test] and .await.
        // This avoids duplicating every template with async variants. (#818)
        if plan.is_async {
            rendered = make_test_async(&rendered);
        }

        output.push_str(&rendered);
        output.push('\n');
    }

    output
}

/// Transform a synchronous test into an async test.
///
/// - `#[test]` → `#[tokio::test]`
/// - `fn {name}()` → `async fn {name}()`
/// - `{fn_name}({args})` gets `.await` appended (on lines with `let` bindings or bare calls)
pub(crate) fn make_test_async(test_code: &str) -> String {
    let mut result = String::new();

    for line in test_code.lines() {
        let transformed = line
            // #[test] → #[tokio::test]
            .replace("#[test]", "#[tokio::test]");

        // fn name() → async fn name()
        let transformed = if transformed.contains("fn ") && transformed.contains("()") {
            transformed.replacen("fn ", "async fn ", 1)
        } else {
            transformed
        };

        // Add .await to function call lines (let result = fn(...); or let _ = fn(...);)
        // but NOT to assert! lines or comment lines
        let transformed = if (transformed.trim_start().starts_with("let ")
            || transformed.trim_start().starts_with("{fn_name}"))
            && transformed.trim_end().ends_with(';')
            && !transformed.contains("assert")
            && !transformed.contains("//")
            && !transformed.contains("Default::default")
        {
            // Insert .await before the trailing semicolon
            if let Some(semi_pos) = transformed.rfind(';') {
                let (before, after) = transformed.split_at(semi_pos);
                format!("{}.await{}", before, after)
            } else {
                transformed
            }
        } else {
            transformed
        };

        result.push_str(&transformed);
        result.push('\n');
    }

    result
}
