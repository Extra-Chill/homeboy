//! Wrapper-to-implementation inference.
//!
//! Detects wrapper files that call implementation functions but don't
//! explicitly declare the relationship. Traces calls in wrapper files
//! against configurable patterns to infer what they wrap.
//!
//! Configuration lives in `homeboy.json` under `audit_rules.wrapper_rules`.

use std::path::Path;

use glob_match::glob_match;
use regex::Regex;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for wrapper inference rules, loaded from homeboy.json.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WrapperInferenceConfig {
    #[serde(default)]
    pub wrapper_rules: Vec<WrapperRule>,
}

/// A single wrapper inference rule.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WrapperRule {
    /// Human-readable rule name (e.g., "tool_ability_link").
    pub name: String,

    /// Glob pattern for wrapper files (e.g., "inc/Engine/AI/Tools/**/*.php").
    pub wrapper_glob: String,

    /// The field/declaration expected in the wrapper (e.g., "ability").
    pub expected_field: String,

    /// Regex patterns to match calls in wrapper files.
    /// Each pattern should have at least one capture group that extracts
    /// the implementation identifier.
    pub call_patterns: Vec<String>,

    /// Optional format string for the suggested fix.
    /// Use `{inferred}` as placeholder for the inferred value.
    /// Default: "'{expected_field}' => '{inferred}'"
    #[serde(default)]
    pub field_format: Option<String>,
}

/// A single call pattern match within a wrapper file.
#[derive(Debug, Clone)]
pub(crate) struct CallMatch {
    pub captured: String,
    pub line_num: Option<usize>,
}

// ============================================================================
// Detection
// ============================================================================

/// Analyze wrapper files for missing implementation declarations.
///
/// For each file matching a wrapper rule's glob:
/// 1. Check if the expected field exists in the file content
/// 2. If not, trace calls against configured patterns
/// 3. Report findings for files with inferred but undeclared implementations
pub(crate) fn analyze_wrappers(fingerprints: &[&FileFingerprint], root: &Path) -> Vec<Finding> {
    let Some(config) = load_config(root) else {
        return Vec::new();
    };

    if config.wrapper_rules.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();

    for rule in &config.wrapper_rules {
        // Compile call patterns once per rule
        let compiled_patterns: Vec<(String, Regex)> = rule
            .call_patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok().map(|r| (p.clone(), r)))
            .collect();

        if compiled_patterns.is_empty() {
            continue;
        }

        for fp in fingerprints {
            // Check if file matches the wrapper glob
            let normalized = fp.relative_path.replace('\\', "/");
            if !glob_match(&rule.wrapper_glob, &normalized) {
                continue;
            }

            // Check if expected field already exists
            if has_field(&fp.content, &rule.expected_field) {
                continue;
            }

            // Trace calls against patterns
            let call_matches = trace_calls(&fp.content, &compiled_patterns);

            if call_matches.is_empty() {
                continue;
            }

            // Deduplicate inferred targets
            let mut inferred_targets: Vec<String> =
                call_matches.iter().map(|m| m.captured.clone()).collect();
            inferred_targets.sort();
            inferred_targets.dedup();

            let suggestion = build_suggestion(rule, &inferred_targets);
            let call_descriptions: Vec<String> = call_matches
                .iter()
                .map(|m| {
                    if let Some(line) = m.line_num {
                        format!("line {}: {}", line, m.captured)
                    } else {
                        m.captured.clone()
                    }
                })
                .collect();

            findings.push(Finding {
                convention: format!("wrapper_inference:{}", rule.name),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: format!(
                    "Wrapper file is missing '{}' declaration. Inferred from calls: {}",
                    rule.expected_field,
                    call_descriptions.join(", ")
                ),
                suggestion,
                kind: AuditFinding::MissingWrapperDeclaration,
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file));
    findings
}

/// Check if the file content already contains the expected field.
fn has_field(content: &str, field_name: &str) -> bool {
    // Check for common declaration patterns:
    // PHP: 'field' => 'value'  or  "field" => "value"
    // JS/TS: field: 'value'  or  field: "value"
    // Rust: field: value
    // YAML: field: value
    content.contains(&format!("'{}' =>", field_name))
        || content.contains(&format!("\"{}\" =>", field_name))
        || content.contains(&format!("{}: ", field_name))
        || content.contains(&format!("\"{}\":", field_name))
        || content.contains(&format!("'{}': ", field_name))
}

/// Trace function calls in file content against compiled regex patterns.
fn trace_calls(content: &str, patterns: &[(String, Regex)]) -> Vec<CallMatch> {
    let mut matches = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        for (_pattern_str, regex) in patterns {
            for cap in regex.captures_iter(line) {
                // Use the first capture group as the inferred target
                if let Some(captured) = cap.get(1) {
                    matches.push(CallMatch {
                        captured: captured.as_str().to_string(),
                        line_num: Some(line_num + 1),
                    });
                }
            }
        }
    }

    matches
}

/// Build a suggestion string for the finding.
fn build_suggestion(rule: &WrapperRule, inferred_targets: &[String]) -> String {
    if inferred_targets.is_empty() {
        return format!("Add '{}' declaration to this wrapper", rule.expected_field);
    }

    let target = if inferred_targets.len() == 1 {
        &inferred_targets[0]
    } else {
        // Multiple targets — show all of them
        return format!(
            "Add '{}' declaration. Multiple implementations detected: {}",
            rule.expected_field,
            inferred_targets.join(", ")
        );
    };

    if let Some(ref format_str) = rule.field_format {
        format_str.replace("{inferred}", target)
    } else {
        format!("Add '{}' => '{}'", rule.expected_field, target)
    }
}

// ============================================================================
// Config loading
// ============================================================================

fn load_config(root: &Path) -> Option<WrapperInferenceConfig> {
    let homeboy_json = root.join("homeboy.json");
    let content = std::fs::read_to_string(homeboy_json).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let audit_rules = value.get("audit_rules")?.clone();
    serde_json::from_value::<WrapperInferenceConfig>(audit_rules).ok()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fingerprint(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn make_rule(name: &str, glob: &str, field: &str, patterns: &[&str]) -> WrapperRule {
        WrapperRule {
            name: name.to_string(),
            wrapper_glob: glob.to_string(),
            expected_field: field.to_string(),
            call_patterns: patterns.iter().map(|p| p.to_string()).collect(),
            field_format: None,
        }
    }

    #[test]
    fn test_has_field_php_style() {
        assert!(has_field("'ability' => 'foo'", "ability"));
        assert!(has_field("\"ability\" => \"foo\"", "ability"));
        assert!(!has_field("'other' => 'foo'", "ability"));
    }

    #[test]
    fn test_has_field_js_style() {
        assert!(has_field("ability: 'foo'", "ability"));
        assert!(has_field("\"ability\": \"foo\"", "ability"));
    }

    #[test]
    fn test_trace_calls_basic() {
        let content = r#"
            $result = wp_get_ability('datamachine/create-pipeline');
            $other = doSomething();
        "#;
        let regex = Regex::new(r"wp_get_ability\('([^']+)'\)").unwrap();
        let patterns = vec![("wp_get_ability".to_string(), regex)];

        let matches = trace_calls(content, &patterns);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].captured, "datamachine/create-pipeline");
    }

    #[test]
    fn test_trace_calls_static_method() {
        let content = r#"
            PipelineAbilities::createPipeline($args);
            LocalSearchAbilities::search($query);
        "#;
        let regex = Regex::new(r"(\w+Abilities)::\w+\(").unwrap();
        let patterns = vec![("abilities_call".to_string(), regex)];

        let matches = trace_calls(content, &patterns);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].captured, "PipelineAbilities");
        assert_eq!(matches[1].captured, "LocalSearchAbilities");
    }

    #[test]
    fn test_trace_calls_no_match() {
        let content = "echo 'hello world';";
        let regex = Regex::new(r"wp_get_ability\('([^']+)'\)").unwrap();
        let patterns = vec![("test".to_string(), regex)];

        let matches = trace_calls(content, &patterns);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_build_suggestion_single_target() {
        let rule = make_rule("test", "*", "ability", &[]);
        let suggestion = build_suggestion(&rule, &["datamachine/search".to_string()]);
        assert_eq!(suggestion, "Add 'ability' => 'datamachine/search'");
    }

    #[test]
    fn test_build_suggestion_with_format() {
        let mut rule = make_rule("test", "*", "ability", &[]);
        rule.field_format = Some("'ability' => '{inferred}'".to_string());
        let suggestion = build_suggestion(&rule, &["datamachine/search".to_string()]);
        assert_eq!(suggestion, "'ability' => 'datamachine/search'");
    }

    #[test]
    fn test_build_suggestion_multiple_targets() {
        let rule = make_rule("test", "*", "ability", &[]);
        let suggestion = build_suggestion(
            &rule,
            &[
                "datamachine/search".to_string(),
                "datamachine/fetch".to_string(),
            ],
        );
        assert!(suggestion.contains("Multiple implementations"));
        assert!(suggestion.contains("datamachine/search"));
        assert!(suggestion.contains("datamachine/fetch"));
    }

    #[test]
    fn test_analyze_wrappers_skips_files_with_field() {
        let content_with_field = "'ability' => 'datamachine/search'\nSearchAbilities::run();";
        let fp = make_fingerprint("tools/Search.php", content_with_field);
        let _fps: Vec<&FileFingerprint> = vec![&fp];

        let _config = WrapperInferenceConfig {
            wrapper_rules: vec![make_rule(
                "test",
                "tools/**/*.php",
                "ability",
                &[r"(\w+Abilities)::\w+\("],
            )],
        };

        // Simulate: file already has the field, should produce no findings
        // (We can't call analyze_wrappers directly without homeboy.json,
        //  so we test the sub-functions)
        assert!(has_field(&fp.content, "ability"));
    }

    #[test]
    fn test_analyze_wrappers_detects_missing_field() {
        let content_missing_field =
            "class CreatePipeline {\n    PipelineAbilities::createPipeline($args);\n}";
        let fp = make_fingerprint("tools/CreatePipeline.php", content_missing_field);

        // File does NOT have 'ability' field
        assert!(!has_field(&fp.content, "ability"));

        // But it DOES call an abilities class
        let regex = Regex::new(r"(\w+Abilities)::\w+\(").unwrap();
        let patterns = vec![("abilities".to_string(), regex)];
        let matches = trace_calls(&fp.content, &patterns);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].captured, "PipelineAbilities");
    }

    #[test]
    fn test_analyze_wrappers_let_some_config_load_config_root_else() {

        let result = analyze_wrappers();
        assert!(!result.is_empty(), "expected non-empty collection for: let Some(config) = load_config(root) else {{");
    }

    #[test]
    fn test_analyze_wrappers_if_let_some_line_m_line_num() {

        let result = analyze_wrappers();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(line) = m.line_num {{");
    }

    #[test]
    fn test_analyze_wrappers_has_expected_effects() {
        // Expected effects: mutation

        let _ = analyze_wrappers();
    }

}
