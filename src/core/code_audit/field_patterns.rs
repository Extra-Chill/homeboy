//! Repeated struct field pattern detection.
//!
//! Finds groups of fields that appear together in multiple struct definitions.
//! When the same fields (same name + type) appear in 3+ structs, they're
//! candidates for extraction into a shared type.
//!
//! Language-agnostic: parses struct field declarations from raw file content
//! using brace-depth tracking and line-level pattern matching. No AST parsing.
//!
//! Examples of what this catches:
//! - `path: Option<String>` with `#[arg(long)]` in 12 `#[derive(Args)]` structs
//! - `verbose: bool` + `quiet: bool` appearing together in 8 CLI structs
//! - Repeated config fields across multiple builder/options types

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};

/// Minimum number of structs sharing a field group to report.
const MIN_OCCURRENCES: usize = 3;

/// Minimum number of fields in a group to report.
const MIN_GROUP_SIZE: usize = 2;

pub(super) fn run(root: &Path) -> Vec<Finding> {
    detect_repeated_field_patterns(root)
}

/// A parsed field from a struct definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FieldSignature {
    /// Field name (e.g., "verbose").
    name: String,
    /// Field type (e.g., "bool", "Option<String>").
    field_type: String,
}

/// A struct and the fields it contains.
struct StructDef {
    /// File containing this struct.
    file: String,
    /// Struct name.
    name: String,
    /// Fields declared in this struct.
    fields: Vec<FieldSignature>,
}

fn detect_repeated_field_patterns(root: &Path) -> Vec<Finding> {
    let config = ScanConfig {
        extensions: ExtensionFilter::Only(vec![
            "rs".to_string(),
            "php".to_string(),
            "ts".to_string(),
            "js".to_string(),
            "go".to_string(),
        ]),
        ..Default::default()
    };
    let files = codebase_scan::walk_files(root, &config);

    let mut all_structs: Vec<StructDef> = Vec::new();

    for file_path in &files {
        let relative = match file_path.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        // Skip test files.
        if is_test_path(&relative) {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        let structs = extract_structs(&content, &relative);
        all_structs.extend(structs);
    }

    // Build a map: field signature → set of (file, struct_name) locations.
    let mut field_locations: HashMap<FieldSignature, Vec<(String, String)>> = HashMap::new();

    for sd in &all_structs {
        for field in &sd.fields {
            field_locations
                .entry(field.clone())
                .or_default()
                .push((sd.file.clone(), sd.name.clone()));
        }
    }

    // Find field GROUPS that co-occur — fields that appear together in
    // the same structs across multiple locations.
    // Strategy: for each pair of fields, check if they always appear together.
    let repeated_fields: Vec<&FieldSignature> = field_locations
        .iter()
        .filter(|(_, locs)| locs.len() >= MIN_OCCURRENCES)
        .map(|(field, _)| field)
        .collect();

    // Group repeated fields by the set of structs they appear in.
    // Fields that appear in the exact same set of structs form a co-occurring group.
    let mut struct_set_to_fields: HashMap<Vec<(String, String)>, Vec<FieldSignature>> =
        HashMap::new();

    for field in &repeated_fields {
        if let Some(locs) = field_locations.get(field) {
            let mut sorted_locs = locs.clone();
            sorted_locs.sort();
            struct_set_to_fields
                .entry(sorted_locs)
                .or_default()
                .push((*field).clone());
        }
    }

    let mut findings = Vec::new();

    for (locations, fields) in &struct_set_to_fields {
        if fields.len() < MIN_GROUP_SIZE {
            continue;
        }
        if locations.len() < MIN_OCCURRENCES {
            continue;
        }

        let field_names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        let struct_names: Vec<String> = locations
            .iter()
            .map(|(file, name)| format!("{}::{}", file, name))
            .collect();

        // Emit one finding per file that contains the pattern.
        let mut seen_files: HashSet<String> = HashSet::new();
        for (file, _) in locations {
            if seen_files.contains(file) {
                continue;
            }
            seen_files.insert(file.clone());

            findings.push(Finding {
                convention: "field_patterns".to_string(),
                severity: Severity::Info,
                file: file.clone(),
                description: format!(
                    "Repeated field group [{}] appears in {} structs: {}",
                    field_names.join(", "),
                    locations.len(),
                    struct_names.join(", ")
                ),
                suggestion: format!(
                    "Extract fields [{}] into a shared struct and flatten/embed it",
                    field_names.join(", ")
                ),
                kind: AuditFinding::RepeatedFieldPattern,
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

/// Extract struct definitions and their fields from file content.
///
/// Language-agnostic: looks for patterns like:
/// - Rust: `struct Name {` ... `field: Type,` ... `}`
/// - PHP: `class Name {` ... `type $field;` or `public type $field;`
/// - TS/JS: `interface Name {` or `type Name = {`
fn extract_structs(content: &str, file: &str) -> Vec<StructDef> {
    let mut result = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();

        // Detect struct/class/interface start.
        let name = extract_type_name(trimmed);
        if let Some(type_name) = name {
            // Find the opening brace (might be on same line or next).
            let brace_line = if trimmed.contains('{') {
                Some(i)
            } else if i + 1 < lines.len() && lines[i + 1].trim().starts_with('{') {
                Some(i + 1)
            } else {
                None
            };

            if let Some(start) = brace_line {
                // Walk to closing brace, tracking depth.
                let mut depth = 0i32;
                let mut fields = Vec::new();
                let mut j = start;

                while j < lines.len() {
                    for ch in lines[j].chars() {
                        match ch {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            _ => {}
                        }
                    }

                    // Parse field declarations from lines inside the struct body.
                    if j > start && depth > 0 {
                        if let Some(field) = parse_field_line(lines[j]) {
                            fields.push(field);
                        }
                    }

                    if depth == 0 && j > start {
                        break;
                    }
                    j += 1;
                }

                if !fields.is_empty() {
                    result.push(StructDef {
                        file: file.to_string(),
                        name: type_name,
                        fields,
                    });
                }

                i = j + 1;
                continue;
            }
        }

        i += 1;
    }

    result
}

/// Try to extract a type name from a line that starts a struct/class/interface.
fn extract_type_name(line: &str) -> Option<String> {
    // Skip comments and attributes.
    let trimmed = line.trim();
    if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("/*") {
        return None;
    }

    // Rust: pub struct Foo, struct Foo, pub(crate) struct Foo
    if let Some(pos) = trimmed.find("struct ") {
        let after = &trimmed[pos + 7..];
        let name = after
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .next()?;
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }

    // PHP/TS: class Foo, interface Foo
    for keyword in &["class ", "interface "] {
        if let Some(pos) = trimmed.find(keyword) {
            let after = &trimmed[pos + keyword.len()..];
            let name = after
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()?;
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }

    None
}

/// Try to parse a field declaration from a single line.
///
/// Returns `Some(FieldSignature)` for lines that look like field declarations.
/// Handles multiple syntax styles:
/// - Rust: `field_name: Type,` or `pub field_name: Type,`
/// - PHP: `public Type $field_name;` or `$field_name;`
/// - TS: `field_name: type;` or `readonly field_name: type;`
fn parse_field_line(line: &str) -> Option<FieldSignature> {
    let trimmed = line.trim();

    // Skip comments, attributes, blank lines, braces.
    if trimmed.is_empty()
        || trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed == "{"
        || trimmed == "}"
        || trimmed == "},"
    {
        return None;
    }

    // Skip function/method declarations.
    if trimmed.contains("fn ") || trimmed.contains("function ") || trimmed.contains("=>") {
        return None;
    }

    // Rust-style: `[pub] name: Type[,]`
    // Strip visibility prefix.
    let content = trimmed
        .strip_prefix("pub(crate) ")
        .or_else(|| trimmed.strip_prefix("pub(super) "))
        .or_else(|| trimmed.strip_prefix("pub "))
        .unwrap_or(trimmed);

    if let Some((name_part, type_part)) = content.split_once(':') {
        let name = name_part.trim().to_string();
        let field_type = type_part
            .trim()
            .trim_end_matches(',')
            .trim_end_matches(';')
            .trim()
            .to_string();

        // Validate name is a reasonable identifier.
        if !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !field_type.is_empty()
        {
            return Some(FieldSignature { name, field_type });
        }
    }

    None
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.starts_with("tests/")
        || lower.starts_with("test/")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_test.php")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.js")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_struct_fields() {
        let content = r#"
pub struct Config {
    pub verbose: bool,
    pub quiet: bool,
    pub output: Option<String>,
}
"#;
        let structs = extract_structs(content, "test.rs");
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name, "Config");
        assert_eq!(structs[0].fields.len(), 3);
        assert_eq!(structs[0].fields[0].name, "verbose");
        assert_eq!(structs[0].fields[0].field_type, "bool");
        assert_eq!(structs[0].fields[2].name, "output");
        assert_eq!(structs[0].fields[2].field_type, "Option<String>");
    }

    #[test]
    fn extracts_multiple_structs() {
        let content = r#"
struct Alpha {
    x: i32,
    y: i32,
}

struct Beta {
    x: i32,
    y: i32,
    z: i32,
}
"#;
        let structs = extract_structs(content, "test.rs");
        assert_eq!(structs.len(), 2);
        assert_eq!(structs[0].name, "Alpha");
        assert_eq!(structs[1].name, "Beta");
    }

    #[test]
    fn skips_methods_inside_struct() {
        let content = r#"
struct Foo {
    name: String,
}

impl Foo {
    fn new() -> Self {
        Self { name: String::new() }
    }
}
"#;
        let structs = extract_structs(content, "test.rs");
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].fields.len(), 1);
        assert_eq!(structs[0].fields[0].name, "name");
    }

    #[test]
    fn parse_field_line_rust() {
        let field = parse_field_line("    pub verbose: bool,");
        assert!(field.is_some());
        let f = field.unwrap();
        assert_eq!(f.name, "verbose");
        assert_eq!(f.field_type, "bool");
    }

    #[test]
    fn parse_field_line_with_option() {
        let field = parse_field_line("    output: Option<PathBuf>,");
        assert!(field.is_some());
        let f = field.unwrap();
        assert_eq!(f.name, "output");
        assert_eq!(f.field_type, "Option<PathBuf>");
    }

    #[test]
    fn parse_field_line_skips_comments() {
        assert!(parse_field_line("    // a comment").is_none());
        assert!(parse_field_line("    #[derive(Debug)]").is_none());
        assert!(parse_field_line("").is_none());
    }

    #[test]
    fn parse_field_line_skips_functions() {
        assert!(parse_field_line("    fn new() -> Self {").is_none());
        assert!(parse_field_line("    pub function run() {").is_none());
    }

    #[test]
    fn extract_type_name_rust() {
        assert_eq!(
            extract_type_name("pub struct Foo {"),
            Some("Foo".to_string())
        );
        assert_eq!(
            extract_type_name("pub(crate) struct Bar<T> {"),
            Some("Bar".to_string())
        );
        assert_eq!(extract_type_name("struct Baz"), Some("Baz".to_string()));
    }

    #[test]
    fn extract_type_name_other_langs() {
        assert_eq!(
            extract_type_name("class MyClass {"),
            Some("MyClass".to_string())
        );
        assert_eq!(
            extract_type_name("interface IFoo {"),
            Some("IFoo".to_string())
        );
    }

    #[test]
    fn extract_type_name_skips_comments() {
        assert_eq!(extract_type_name("// struct Comment"), None);
        assert_eq!(extract_type_name("# class PythonStyle"), None);
    }

    #[test]
    fn detects_repeated_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        // Three files with the same field group.
        for name in &["alpha.rs", "beta.rs", "gamma.rs"] {
            std::fs::write(
                src.join(name),
                format!(
                    "struct {} {{\n    verbose: bool,\n    quiet: bool,\n}}\n",
                    name.replace(".rs", "").to_uppercase()
                ),
            )
            .unwrap();
        }

        let findings = detect_repeated_field_patterns(dir.path());
        assert!(
            !findings.is_empty(),
            "Should detect repeated [verbose, quiet] pattern"
        );
        assert!(findings
            .iter()
            .all(|f| f.kind == AuditFinding::RepeatedFieldPattern));
    }

    #[test]
    fn ignores_below_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        // Only 2 files (below MIN_OCCURRENCES=3).
        for name in &["alpha.rs", "beta.rs"] {
            std::fs::write(
                src.join(name),
                "struct Foo {\n    x: i32,\n    y: i32,\n}\n",
            )
            .unwrap();
        }

        let findings = detect_repeated_field_patterns(dir.path());
        assert!(
            findings.is_empty(),
            "Two occurrences should be below threshold"
        );
    }

    #[test]
    fn test_run_default_path() {

        let result = run();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

}
