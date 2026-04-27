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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldSyntax {
    RustLike,
    Php,
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

        let scan_content = if relative.ends_with(".rs") {
            strip_rust_cfg_test_modules(&content)
        } else {
            content
        };

        let syntax = field_syntax_for_path(&relative);
        let structs = extract_structs(&scan_content, &relative, syntax);
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
    let mut repeated_fields: Vec<&FieldSignature> = field_locations
        .iter()
        .filter(|(_, locs)| locs.len() >= MIN_OCCURRENCES)
        .map(|(field, _)| field)
        .collect();
    repeated_fields.sort_by(|a, b| a.name.cmp(&b.name).then(a.field_type.cmp(&b.field_type)));

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

    let mut grouped_entries: Vec<(&Vec<(String, String)>, &Vec<FieldSignature>)> =
        struct_set_to_fields.iter().collect();
    grouped_entries.sort_by(|a, b| a.0.cmp(b.0));

    for (locations, fields) in grouped_entries {
        if fields.len() < MIN_GROUP_SIZE {
            continue;
        }
        if locations.len() < MIN_OCCURRENCES {
            continue;
        }

        let mut sorted_fields = fields.clone();
        sorted_fields.sort_by(|a, b| a.name.cmp(&b.name).then(a.field_type.cmp(&b.field_type)));
        let field_names: Vec<&str> = sorted_fields.iter().map(|f| f.name.as_str()).collect();
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
fn extract_structs(content: &str, file: &str, syntax: FieldSyntax) -> Vec<StructDef> {
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

                    // Parse only direct members of the type body. Nested executable bodies
                    // can contain field-shaped syntax that is not extractable type structure.
                    if j > start && depth == 1 {
                        if let Some(field) = parse_field_line(lines[j], syntax) {
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

fn field_syntax_for_path(path: &str) -> FieldSyntax {
    if path.ends_with(".php") {
        FieldSyntax::Php
    } else {
        FieldSyntax::RustLike
    }
}

/// Try to extract a type name from a line that starts a struct/class/interface.
fn extract_type_name(line: &str) -> Option<String> {
    // Skip comments and attributes.
    let mut trimmed = line.trim();
    if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("/*") {
        return None;
    }

    loop {
        let Some(stripped) = trimmed
            .strip_prefix("pub(crate) ")
            .or_else(|| trimmed.strip_prefix("pub(super) "))
            .or_else(|| trimmed.strip_prefix("pub "))
            .or_else(|| trimmed.strip_prefix("export "))
            .or_else(|| trimmed.strip_prefix("default "))
            .or_else(|| trimmed.strip_prefix("abstract "))
            .or_else(|| trimmed.strip_prefix("final "))
        else {
            break;
        };
        trimmed = stripped.trim_start();
    }

    // Rust: pub struct Foo, struct Foo, pub(crate) struct Foo.
    if let Some(after) = trimmed.strip_prefix("struct ") {
        let name = after
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .next()?;
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }

    // PHP/TS: class Foo, interface Foo
    for keyword in &["class ", "interface "] {
        if let Some(after) = trimmed.strip_prefix(keyword) {
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
fn parse_field_line(line: &str, syntax: FieldSyntax) -> Option<FieldSignature> {
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

    if syntax == FieldSyntax::Php {
        return parse_php_property_line(trimmed);
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

fn parse_php_property_line(line: &str) -> Option<FieldSignature> {
    let mut content = line.trim().trim_end_matches(';').trim();

    loop {
        let Some(stripped) = content
            .strip_prefix("public ")
            .or_else(|| content.strip_prefix("protected "))
            .or_else(|| content.strip_prefix("private "))
            .or_else(|| content.strip_prefix("static "))
            .or_else(|| content.strip_prefix("readonly "))
        else {
            break;
        };
        content = stripped.trim_start();
    }

    let Some(dollar_pos) = content.find('$') else {
        return None;
    };

    let field_type = content[..dollar_pos].trim();
    let after_dollar = &content[dollar_pos + 1..];
    let name: String = after_dollar
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if name.is_empty() {
        return None;
    }

    let field_type = if field_type.is_empty() {
        "mixed"
    } else {
        field_type
    };

    Some(FieldSignature {
        name,
        field_type: field_type.to_string(),
    })
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

fn strip_rust_cfg_test_modules(content: &str) -> String {
    let mut out = Vec::new();
    let mut pending_cfg_test: Option<&str> = None;
    let mut skipping = false;
    let mut depth = 0i32;
    let mut raw_string_hashes: Option<usize> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if skipping {
            depth += brace_delta_outside_rust_raw_strings(line, &mut raw_string_hashes);
            if depth <= 0 {
                skipping = false;
                raw_string_hashes = None;
            }
            continue;
        }

        if let Some(cfg_line) = pending_cfg_test.take() {
            if trimmed.starts_with("mod tests") {
                skipping = true;
                raw_string_hashes = None;
                depth = brace_delta_outside_rust_raw_strings(line, &mut raw_string_hashes);
                if depth <= 0 {
                    skipping = false;
                    raw_string_hashes = None;
                }
                continue;
            }

            out.push(cfg_line.to_string());
        }

        if trimmed == "#[cfg(test)]" {
            pending_cfg_test = Some(line);
            continue;
        }

        out.push(line.to_string());
    }

    if let Some(cfg_line) = pending_cfg_test {
        out.push(cfg_line.to_string());
    }

    out.join("\n")
}

fn brace_delta_outside_rust_raw_strings(line: &str, raw_hashes: &mut Option<usize>) -> i32 {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut depth = 0;

    while i < bytes.len() {
        if let Some(hashes) = *raw_hashes {
            if raw_string_closes_at(bytes, i, hashes) {
                *raw_hashes = None;
                i += 1 + hashes;
            } else {
                i += 1;
            }
            continue;
        }

        if let Some(hashes) = raw_string_opens_at(bytes, i) {
            *raw_hashes = Some(hashes);
            i += 2 + hashes;
            continue;
        }

        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }

    depth
}

fn raw_string_opens_at(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'r') {
        return None;
    }

    let mut i = start + 1;
    while bytes.get(i) == Some(&b'#') {
        i += 1;
    }

    if bytes.get(i) == Some(&b'"') {
        Some(i - start - 1)
    } else {
        None
    }
}

fn raw_string_closes_at(bytes: &[u8], start: usize, hashes: usize) -> bool {
    if bytes.get(start) != Some(&b'"') {
        return false;
    }

    (0..hashes).all(|offset| bytes.get(start + 1 + offset) == Some(&b'#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run(dir.path()).is_empty());
    }

    #[test]
    fn extracts_rust_struct_fields() {
        let content = r#"
pub struct Config {
    pub verbose: bool,
    pub quiet: bool,
    pub output: Option<String>,
}
"#;
        let structs = extract_structs(content, "test.rs", FieldSyntax::RustLike);
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
        let structs = extract_structs(content, "test.rs", FieldSyntax::RustLike);
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
        let structs = extract_structs(content, "test.rs", FieldSyntax::RustLike);
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].fields.len(), 1);
        assert_eq!(structs[0].fields[0].name, "name");
    }

    #[test]
    fn skips_call_arguments_inside_php_methods() {
        let content = r#"
class AIStep {
    public static function register(): void {
        self::registerStepType(
            class_name: self::class,
            label: 'AI',
        );
        add_filter('datamachine_handlers', [self::class, 'register']);
    }
}
"#;

        let structs = extract_structs(content, "test.php", FieldSyntax::Php);
        assert!(
            structs.is_empty(),
            "call-site named arguments inside methods should not create field-bearing structs"
        );
    }

    #[test]
    fn extracts_php_class_properties_without_named_arguments() {
        let content = r#"
class Config {
    public string $label;
    protected ?array $settings;

    public static function register(): void {
        self::registerStepType(
            label: 'Config',
            stepSettings: array(),
        );
    }
}
"#;

        let structs = extract_structs(content, "test.php", FieldSyntax::Php);
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].fields.len(), 2);
        assert_eq!(structs[0].fields[0].name, "label");
        assert_eq!(structs[0].fields[0].field_type, "string");
        assert_eq!(structs[0].fields[1].name, "settings");
        assert_eq!(structs[0].fields[1].field_type, "?array");
    }

    #[test]
    fn does_not_report_repeated_php_presentation_or_command_call_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        for name in &["callback.php", "authorize.php", "webhook.php"] {
            std::fs::write(
                src.join(name),
                format!(
                    r#"
class {} {{
    public function render(): void {{
        $styles = array(
            'display' => 'flex',
            'background' => '#fff',
        );
        WP_CLI::log( 'Rendered' );
    }}
}}
"#,
                    name.replace(".php", "").to_uppercase()
                ),
            )
            .unwrap();
        }

        let findings = detect_repeated_field_patterns(dir.path());
        assert!(
            findings.is_empty(),
            "presentation arrays and WP_CLI call sites are not extractable field groups: {:?}",
            findings
        );
    }

    #[test]
    fn skips_field_shaped_syntax_inside_rust_methods() {
        let content = r#"
struct Foo {
    name: String,
}

impl Foo {
    fn new() -> Self {
        Self {
            name: String::new(),
            label: "nested literal",
        }
    }
}
"#;

        let structs = extract_structs(content, "test.rs", FieldSyntax::RustLike);
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].fields.len(), 1);
        assert_eq!(structs[0].fields[0].name, "name");
        assert_eq!(structs[0].fields[0].field_type, "String");
    }

    #[test]
    fn skips_field_shaped_syntax_inside_typescript_methods() {
        let content = r#"
class Widget {
    name: string;

    build() {
        return {
            name: 'nested literal',
            label: 'not a class field',
        };
    }
}
"#;

        let structs = extract_structs(content, "test.ts", FieldSyntax::RustLike);
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].fields.len(), 1);
        assert_eq!(structs[0].fields[0].name, "name");
        assert_eq!(structs[0].fields[0].field_type, "string");
    }

    #[test]
    fn does_not_report_repeated_php_self_registration_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        for name in &["ai.php", "fetch.php", "publish.php"] {
            std::fs::write(
                src.join(name),
                format!(
                    r#"
class {} {{
    public static function register(): void {{
        self::registerStepType(
            class_name: self::class,
            label: 'Step',
        );
    }}
}}
"#,
                    name.replace(".php", "").to_uppercase()
                ),
            )
            .unwrap();
        }

        let findings = detect_repeated_field_patterns(dir.path());
        assert!(
            findings.is_empty(),
            "PHP self::class registration arguments should not be reported as fields"
        );
    }

    #[test]
    fn skips_rust_cfg_test_modules_when_scanning_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("detector.rs"),
            r##"
pub struct RealConfig {
    enabled: bool,
}

#[cfg(test)]
mod tests {
    #[test]
    fn fixture_strings_do_not_count_as_real_structs() {
        let _ = r#"
struct Foo {
    std: fs::PathBuf,
    std: io::Result<()>,
}
"#;
    }

    #[test]
    fn fixture_two() {
        let _ = r#"
struct Foo {
    std: fs::PathBuf,
    std: io::Result<()>,
}
"#;
    }

    #[test]
    fn fixture_three() {
        let _ = r#"
struct Foo {
    std: fs::PathBuf,
    std: io::Result<()>,
}
"#;
    }
}
"##,
        )
        .unwrap();

        let findings = detect_repeated_field_patterns(dir.path());
        assert!(
            findings.is_empty(),
            "inline Rust test fixtures should not be scanned as production structs: {:?}",
            findings
        );
    }

    #[test]
    fn parse_field_line_rust() {
        let field = parse_field_line("    pub verbose: bool,", FieldSyntax::RustLike);
        assert!(field.is_some());
        let f = field.unwrap();
        assert_eq!(f.name, "verbose");
        assert_eq!(f.field_type, "bool");
    }

    #[test]
    fn parse_field_line_with_option() {
        let field = parse_field_line("    output: Option<PathBuf>,", FieldSyntax::RustLike);
        assert!(field.is_some());
        let f = field.unwrap();
        assert_eq!(f.name, "output");
        assert_eq!(f.field_type, "Option<PathBuf>");
    }

    #[test]
    fn parse_field_line_skips_comments() {
        assert!(parse_field_line("    // a comment", FieldSyntax::RustLike).is_none());
        assert!(parse_field_line("    #[derive(Debug)]", FieldSyntax::RustLike).is_none());
        assert!(parse_field_line("", FieldSyntax::RustLike).is_none());
    }

    #[test]
    fn parse_field_line_skips_functions() {
        assert!(parse_field_line("    fn new() -> Self {", FieldSyntax::RustLike).is_none());
        assert!(parse_field_line("    pub function run() {", FieldSyntax::RustLike).is_none());
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
    fn extract_type_name_skips_keywords_inside_string_literals() {
        assert_eq!(extract_type_name("let content = \"struct Foo {\";"), None);
        assert_eq!(extract_type_name("format!(\"class Widget {\")"), None);
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
        assert_eq!(
            extract_type_name("export interface Props {"),
            Some("Props".to_string())
        );
        assert_eq!(
            extract_type_name("export default class Widget {"),
            Some("Widget".to_string())
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
    fn repeated_pattern_description_orders_fields_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        for name in &["alpha.rs", "beta.rs", "gamma.rs"] {
            std::fs::write(
                src.join(name),
                format!(
                    "struct {} {{\n    zebra: bool,\n    alpha: bool,\n    middle: bool,\n}}\n",
                    name.replace(".rs", "").to_uppercase()
                ),
            )
            .unwrap();
        }

        let findings = detect_repeated_field_patterns(dir.path());
        assert!(
            !findings.is_empty(),
            "Should detect repeated [alpha, middle, zebra] pattern"
        );
        assert!(
            findings.iter().all(|f| f
                .description
                .contains("Repeated field group [alpha, middle, zebra]")),
            "field order in descriptions must be stable and lexical: {:?}",
            findings
                .iter()
                .map(|f| f.description.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            findings
                .iter()
                .all(|f| f.suggestion.contains("[alpha, middle, zebra]")),
            "field order in suggestions must match descriptions"
        );
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
}
