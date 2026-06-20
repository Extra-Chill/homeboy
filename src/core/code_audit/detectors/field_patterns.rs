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

#[rustfmt::skip]
mod field_patterns_data_contracts {
    const TYPE_NAMES: &[&str] = &["Component", "Convention", "DirectoryConvention", "FileFingerprint", "Insertion", "MapClass", "NewFile", "Project", "RawComponent"];
    const TYPE_SUFFIXES: &[&str] = &["Args", "Buckets", "CommandInput", "Detail", "Drift", "EditOp", "Entry", "Flags", "Group", "Options", "Output", "Overrides", "Report", "Result", "SeverityCounts", "Snapshot", "Status", "Summary"];
    const LOW_VALUE_FIELDS: &[&str] = &["ahead", "behind", "build_artifact", "changelog_next_section_aliases", "changelog_next_section_label", "confidence", "deploy", "deploy_strategy", "docs_only", "expected_methods", "expected_registrations", "extends", "extract_command", "failure", "implements", "info", "manual_only", "namespace", "needs_release", "picked_count", "primitive", "properties", "ready_detail", "ready_reason", "ready_to_deploy", "remote_owner", "results", "runtime", "skip_checks", "skip_publish", "skipped_count", "summary", "warnings"];

    pub(super) fn is_low_value_group(field_names: &[&str], type_names: &[&str], min_group_size: usize, min_occurrences: usize) -> bool {
        (min_group_size..=4).contains(&field_names.len())
            && type_names.len() >= min_occurrences
            && type_names.iter().all(|name| TYPE_NAMES.contains(name) || TYPE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)))
            && field_names.iter().all(|name| LOW_VALUE_FIELDS.contains(name))
    }
}

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::core::component::DetectorProfileConfig;
use crate::core::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
#[allow(unused_imports)]
use super::fingerprint::FileFingerprint;

/// Minimum number of structs sharing a field group to report.
const MIN_OCCURRENCES: usize = 3;

/// Minimum number of fields in a group to report.
const MIN_GROUP_SIZE: usize = 2;

pub(in crate::core::code_audit) fn run(
    root: &Path,
    config: &DetectorProfileConfig,
) -> Vec<Finding> {
    let settings = ResolvedFieldScan::from_config(config);
    if settings.scan_tokens.is_empty() {
        // No scan tokens resolved (builtin defaults disabled and none declared):
        // core ships no built-in ecosystem scan list inside the detector, so the
        // detector stays inert until a component declares its languages.
        return Vec::new();
    }
    detect_repeated_field_patterns(root, &settings)
}

/// Resolved field-scan settings. Concrete language/extension tokens come from
/// [`DetectorProfileConfig`] (or the agnostic builtin token catalogue when a
/// component opts into defaults); this detector keeps no hardcoded ecosystem
/// literals of its own.
struct ResolvedFieldScan {
    scan_tokens: Vec<String>,
    type_before_name_tokens: Vec<String>,
    inline_test_strip_tokens: Vec<String>,
    test_file_suffixes: Vec<String>,
}

impl ResolvedFieldScan {
    fn from_config(config: &DetectorProfileConfig) -> Self {
        let scan_tokens = if !config.field_pattern_scan_tokens.is_empty() {
            config.field_pattern_scan_tokens.clone()
        } else if config.use_builtin_defaults {
            Language::builtin_extension_tokens()
                .iter()
                .map(|token| (*token).to_string())
                .collect()
        } else {
            Vec::new()
        };
        Self {
            scan_tokens,
            type_before_name_tokens: config.field_pattern_type_before_name_tokens.clone(),
            inline_test_strip_tokens: config.field_pattern_inline_test_strip_tokens.clone(),
            test_file_suffixes: config.test_file_suffixes.clone(),
        }
    }

    fn syntax_for_path(&self, path: &str) -> FieldSyntax {
        let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if self
            .type_before_name_tokens
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext))
        {
            FieldSyntax::TypeBeforeName
        } else {
            FieldSyntax::NameBeforeType
        }
    }

    fn needs_inline_test_strip(&self, path: &str) -> bool {
        let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        self.inline_test_strip_tokens
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext))
    }

    fn is_test_path(&self, path: &str) -> bool {
        let lower = path.to_lowercase();
        lower.contains("/tests/")
            || lower.contains("/test/")
            || lower.starts_with("tests/")
            || lower.starts_with("test/")
            || self
                .test_file_suffixes
                .iter()
                .any(|suffix| lower.ends_with(&suffix.to_lowercase()))
    }
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

/// Syntactic family a file's field declarations follow. Core distinguishes
/// only the shape, not the ecosystem: `NameBeforeType` covers `name: Type`
/// declarations; `TypeBeforeName` covers `Type $name` / `Type name`
/// declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldSyntax {
    NameBeforeType,
    TypeBeforeName,
}

fn detect_repeated_field_patterns(root: &Path, settings: &ResolvedFieldScan) -> Vec<Finding> {
    let config = ScanConfig {
        extensions: ExtensionFilter::Only(settings.scan_tokens.clone()),
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
        if settings.is_test_path(&relative) {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        let scan_content = if settings.needs_inline_test_strip(&relative) {
            strip_rust_cfg_test_modules(&content)
        } else {
            content
        };

        let syntax = settings.syntax_for_path(&relative);
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
        if is_boundary_dto_group_across_layers(locations) {
            continue;
        }
        if is_low_value_boundary_coordinate_group(&sorted_fields, locations) {
            continue;
        }
        if field_patterns_data_contracts::is_low_value_group(
            &sorted_fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            &locations
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            MIN_GROUP_SIZE,
            MIN_OCCURRENCES,
        ) {
            continue;
        }
        if is_low_value_generic_group(&sorted_fields, locations) {
            continue;
        }
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
/// Language-agnostic: looks for aggregate/record declarations and their
/// member fields across the two supported syntactic families:
/// - name-before-type: `struct Name {` ... `field: Type,` ... `}` and
///   `interface Name {` / `type Name = {`
/// - type-before-name: `class Name {` ... `Type $field;` /
///   `public Type $field;`
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

    // Record keyword forms: `[pub] struct Foo`.
    if let Some(after) = trimmed.strip_prefix("struct ") {
        return parse_identifier(after);
    }

    // Class/interface keyword forms: `class Foo`, `interface Foo`.
    for keyword in &["class ", "interface "] {
        if let Some(after) = trimmed.strip_prefix(keyword) {
            return parse_identifier(after);
        }
    }

    None
}

fn parse_identifier(input: &str) -> Option<String> {
    let name = input
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Try to parse a field declaration from a single line.
///
/// Returns `Some(FieldSignature)` for lines that look like field declarations.
/// Handles both syntactic families:
/// - name-before-type: `field_name: Type,` or `pub field_name: Type,` and
///   `field_name: type;` / `readonly field_name: type;`
/// - type-before-name: `public Type $field_name;` or `$field_name;`
fn parse_field_line(line: &str, _syntax: FieldSyntax) -> Option<FieldSignature> {
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

    match _syntax {
        FieldSyntax::TypeBeforeName => return parse_type_before_name_field(trimmed),
        FieldSyntax::NameBeforeType => {}
    }

    // Name-before-type: `[pub] name: Type[,]`
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

fn parse_type_before_name_field(line: &str) -> Option<FieldSignature> {
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

    let dollar_pos = content.find('$')?;

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

fn is_low_value_generic_group(fields: &[FieldSignature], locations: &[(String, String)]) -> bool {
    if fields.len() != 2 {
        return false;
    }

    let mut names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    names.sort_unstable();
    let is_generic_pair = matches!(
        names.as_slice(),
        ["from", "to"]
            | ["host", "port"]
            | ["local_version", "remote_version"]
            | ["new_version", "old_version"]
            | ["stderr", "stdout"]
    );
    if !is_generic_pair {
        return false;
    }

    let module = |file: &str| {
        file.rsplit_once('/')
            .map(|(module, _)| module)
            .unwrap_or("")
            .to_string()
    };
    let shared_module = locations
        .first()
        .map(|(file, _)| {
            locations
                .iter()
                .all(|(other, _)| module(other) == module(file))
        })
        .unwrap_or(false);
    if shared_module {
        return false;
    }

    let suffix = |name: &str| {
        let start = name
            .char_indices()
            .filter_map(|(index, ch)| ch.is_uppercase().then_some(index))
            .next_back()?;
        let suffix = &name[start..];
        let generic_suffix = matches!(
            suffix,
            "Client" | "Config" | "Output" | "Result" | "Row" | "Server" | "Summary"
        );
        (suffix.len() > 2 && !generic_suffix).then_some(suffix.to_string())
    };

    let shared_suffix = locations
        .first()
        .and_then(|(_, name)| suffix(name))
        .map(|first| {
            locations
                .iter()
                .all(|(_, name)| suffix(name).as_ref() == Some(&first))
        })
        .unwrap_or(false);
    !shared_suffix
}

fn is_boundary_dto_group_across_layers(locations: &[(String, String)]) -> bool {
    let mut layers = HashSet::new();

    for (file, name) in locations {
        if !is_boundary_dto_name(name) {
            return false;
        }
        let Some(layer) = boundary_layer(file) else {
            return false;
        };
        layers.insert(layer);
    }

    layers.len() > 1
}

fn is_boundary_dto_name(name: &str) -> bool {
    matches!(
        name,
        "Args" | "Options" | "Record" | "WorkflowArgs" | "WorkflowOptions"
    ) || name.ends_with("Args")
        || name.ends_with("Options")
        || name.ends_with("Record")
        || name.ends_with("WorkflowArgs")
        || name.ends_with("WorkflowOptions")
}

fn is_low_value_boundary_coordinate_group(
    fields: &[FieldSignature],
    locations: &[(String, String)],
) -> bool {
    if fields.len() != 2 || locations.len() < MIN_OCCURRENCES {
        return false;
    }

    let mut names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    names.sort_unstable();
    if names != ["fixable", "line"] {
        return false;
    }

    let Some((first_file, _)) = locations.first() else {
        return false;
    };

    locations
        .iter()
        .all(|(file, name)| file == first_file && is_boundary_dto_name(name))
}

fn boundary_layer(file: &str) -> Option<&'static str> {
    if file.starts_with("src/commands/") {
        Some("command")
    } else if file.starts_with("src/core/extension/") {
        Some("workflow")
    } else if file.starts_with("src/core/refactor/") {
        Some("refactor")
    } else {
        None
    }
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
            advance_cfg_test_skip(line, &mut skipping, &mut depth, &mut raw_string_hashes);
            continue;
        }

        if let Some(cfg_line) = pending_cfg_test.take() {
            if trimmed.starts_with("mod tests") {
                start_cfg_test_skip(line, &mut skipping, &mut depth, &mut raw_string_hashes);
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

fn start_cfg_test_skip(
    line: &str,
    skipping: &mut bool,
    depth: &mut i32,
    raw_string_hashes: &mut Option<usize>,
) {
    *skipping = true;
    *raw_string_hashes = None;
    *depth = brace_delta_outside_rust_raw_strings(line, raw_string_hashes);
    if *depth <= 0 {
        finish_cfg_test_skip(skipping, raw_string_hashes);
    }
}

fn advance_cfg_test_skip(
    line: &str,
    skipping: &mut bool,
    depth: &mut i32,
    raw_string_hashes: &mut Option<usize>,
) {
    *depth += brace_delta_outside_rust_raw_strings(line, raw_string_hashes);
    if *depth <= 0 {
        finish_cfg_test_skip(skipping, raw_string_hashes);
    }
}

fn finish_cfg_test_skip(skipping: &mut bool, raw_string_hashes: &mut Option<usize>) {
    *skipping = false;
    *raw_string_hashes = None;
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

    /// Representative multi-language scan settings for tests, mirroring what an
    /// extension/component profile would declare.
    fn test_scan() -> ResolvedFieldScan {
        ResolvedFieldScan {
            scan_tokens: vec![
                "rs".to_string(),
                "php".to_string(),
                "ts".to_string(),
                "js".to_string(),
                "go".to_string(),
            ],
            type_before_name_tokens: vec!["php".to_string()],
            inline_test_strip_tokens: vec!["rs".to_string()],
            test_file_suffixes: vec![
                "_test.rs".to_string(),
                "_test.php".to_string(),
                ".test.ts".to_string(),
                ".test.js".to_string(),
            ],
        }
    }

    #[test]
    fn test_run() {
        let dir = tempfile::tempdir().unwrap();
        // Empty profile (no tokens, builtin defaults on) → still empty on empty dir.
        assert!(run(dir.path(), &DetectorProfileConfig::default()).is_empty());
    }

    #[test]
    fn run_is_inert_without_scan_tokens() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "struct A { verbose: bool, quiet: bool }\nstruct B { verbose: bool, quiet: bool }\nstruct C { verbose: bool, quiet: bool }\n",
        )
        .unwrap();
        let config = DetectorProfileConfig {
            use_builtin_defaults: false,
            ..Default::default()
        };
        assert!(
            run(dir.path(), &config).is_empty(),
            "no scan tokens declared and builtin defaults off → inert"
        );
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
        let structs = extract_structs(content, "test.rs", FieldSyntax::NameBeforeType);
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
        let structs = extract_structs(content, "test.rs", FieldSyntax::NameBeforeType);
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
        let structs = extract_structs(content, "test.rs", FieldSyntax::NameBeforeType);
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
        add_filter('sampleplugin_handlers', [self::class, 'register']);
    }
}
"#;

        let structs = extract_structs(content, "test.php", FieldSyntax::TypeBeforeName);
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

        let structs = extract_structs(content, "test.php", FieldSyntax::TypeBeforeName);
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

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
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

        let structs = extract_structs(content, "test.rs", FieldSyntax::NameBeforeType);
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

        let structs = extract_structs(content, "test.ts", FieldSyntax::NameBeforeType);
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

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
        assert!(
            findings.is_empty(),
            "PHP self::class registration arguments should not be reported as fields"
        );
    }

    #[test]
    fn does_not_report_repeated_boundary_record_coordinates() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/core/observation");
        std::fs::create_dir_all(&src).unwrap();

        std::fs::write(
            src.join("records.rs"),
            r#"
struct AnnotationFindingRecord {
    line: Option<u32>,
    fixable: bool,
    annotation_id: String,
}

struct NewFindingRecord {
    line: Option<u32>,
    fixable: bool,
    run_id: String,
}

struct FindingRecord {
    line: Option<u32>,
    fixable: bool,
    id: String,
}
"#,
        )
        .unwrap();

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
        assert!(
            findings.is_empty(),
            "small scalar coordinate overlaps across boundary records are not extractable: {:?}",
            findings
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

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
        assert!(
            findings.is_empty(),
            "inline Rust test fixtures should not be scanned as production structs: {:?}",
            findings
        );
    }

    #[test]
    fn parse_field_line_rust() {
        let field = parse_field_line("    pub verbose: bool,", FieldSyntax::NameBeforeType);
        assert!(field.is_some());
        let f = field.unwrap();
        assert_eq!(f.name, "verbose");
        assert_eq!(f.field_type, "bool");
    }

    #[test]
    fn parse_field_line_with_option() {
        let field = parse_field_line("    output: Option<PathBuf>,", FieldSyntax::NameBeforeType);
        assert!(field.is_some());
        let f = field.unwrap();
        assert_eq!(f.name, "output");
        assert_eq!(f.field_type, "Option<PathBuf>");
    }

    #[test]
    fn parse_field_line_skips_comments() {
        assert!(parse_field_line("    // a comment", FieldSyntax::NameBeforeType).is_none());
        assert!(parse_field_line("    #[derive(Debug)]", FieldSyntax::NameBeforeType).is_none());
        assert!(parse_field_line("", FieldSyntax::NameBeforeType).is_none());
    }

    #[test]
    fn parse_field_line_skips_functions() {
        assert!(parse_field_line("    fn new() -> Self {", FieldSyntax::NameBeforeType).is_none());
        assert!(
            parse_field_line("    pub function run() {", FieldSyntax::NameBeforeType).is_none()
        );
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

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
        assert!(
            !findings.is_empty(),
            "Should detect repeated [verbose, quiet] pattern"
        );
        assert!(findings
            .iter()
            .all(|f| f.kind == AuditFinding::RepeatedFieldPattern));
    }

    #[test]
    fn suppresses_boundary_dto_field_overlap_across_layers() {
        let dir = tempfile::tempdir().unwrap();
        let commands = dir.path().join("src/commands");
        let extension = dir.path().join("src/core/extension/lint");
        let refactor = dir.path().join("src/core/refactor/plan");
        std::fs::create_dir_all(&commands).unwrap();
        std::fs::create_dir_all(&extension).unwrap();
        std::fs::create_dir_all(&refactor).unwrap();

        std::fs::write(
            commands.join("lint.rs"),
            r#"
struct LintArgs {
    category: Option<String>,
    errors_only: bool,
    exclude_sniffs: Option<String>,
    glob: Option<String>,
    sniffs: Option<String>,
    changed_only: bool,
    summary: bool,
}
"#,
        )
        .unwrap();
        std::fs::write(
            extension.join("run.rs"),
            r#"
struct LintRunWorkflowArgs {
    category: Option<String>,
    errors_only: bool,
    exclude_sniffs: Option<String>,
    glob: Option<String>,
    sniffs: Option<String>,
    changed_only: bool,
    summary: bool,
}
"#,
        )
        .unwrap();
        std::fs::write(
            refactor.join("sources.rs"),
            r#"
struct LintSourceOptions {
    category: Option<String>,
    errors_only: bool,
    exclude_sniffs: Option<String>,
    glob: Option<String>,
    sniffs: Option<String>,
}
"#,
        )
        .unwrap();
        std::fs::write(
            commands.join("review.rs"),
            r#"
struct ReviewArgs {
    changed_only: bool,
    summary: bool,
}
"#,
        )
        .unwrap();

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
        assert!(
            findings.is_empty(),
            "boundary DTO overlap across command/workflow/refactor layers should not suggest extraction: {:?}",
            findings
        );
    }

    #[test]
    fn keeps_boundary_dto_signal_inside_one_layer() {
        let dir = tempfile::tempdir().unwrap();
        let commands = dir.path().join("src/commands");
        std::fs::create_dir_all(&commands).unwrap();

        for name in &["AuditArgs", "LintArgs", "TestArgs"] {
            std::fs::write(
                commands.join(format!("{}.rs", name.to_lowercase())),
                format!("struct {name} {{\n    dry_run: bool,\n    output: Option<String>,\n}}\n"),
            )
            .unwrap();
        }

        let descriptions: Vec<String> = detect_repeated_field_patterns(dir.path(), &test_scan())
            .into_iter()
            .map(|finding| finding.description)
            .collect();
        assert!(
            descriptions
                .iter()
                .any(|description| description.contains("[dry_run, output]")),
            "boundary DTOs inside one layer can still be useful local extraction signals: {:?}",
            descriptions
        );
    }

    #[test]
    fn suppresses_generic_pairs_across_unrelated_modules() {
        let dir = tempfile::tempdir().unwrap();

        for (module, name, fields) in [
            (
                "ssh",
                "SshConnectOutput",
                "stdout: String,\n    stderr: String,",
            ),
            ("db", "DbResult", "stdout: String,\n    stderr: String,"),
            (
                "fleet",
                "FleetExecProjectResult",
                "stdout: String,\n    stderr: String,",
            ),
            (
                "database",
                "DatabaseConfig",
                "host: String,\n    port: u16,",
            ),
            ("server", "Server", "host: String,\n    port: u16,"),
            ("client", "SshClient", "host: String,\n    port: u16,"),
            ("rename", "RenameSummary", "from: String,\n    to: String,"),
            (
                "variant",
                "VariantSummary",
                "from: String,\n    to: String,",
            ),
            ("file", "FileRename", "from: String,\n    to: String,"),
            (
                "deploy",
                "DeployStatusRow",
                "local_version: String,\n    remote_version: String,",
            ),
            (
                "fleet_status",
                "FleetStatusRow",
                "local_version: String,\n    remote_version: String,",
            ),
            (
                "release",
                "ReleaseStatusRow",
                "local_version: String,\n    remote_version: String,",
            ),
        ] {
            let module_dir = dir.path().join("src").join(module);
            std::fs::create_dir_all(&module_dir).unwrap();
            std::fs::write(
                module_dir.join("types.rs"),
                format!("struct {name} {{\n    {fields}\n}}\n"),
            )
            .unwrap();
        }

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
        assert!(
            findings.is_empty(),
            "generic DTO field pairs across unrelated modules should not become extraction work: {:?}",
            findings
        );
    }

    #[test]
    fn suppresses_low_value_data_contract_field_overlap() {
        let dir = tempfile::tempdir().unwrap();

        for (path, name, fields) in [
            (
                "src/commands/deploy.rs",
                "DeployOutput",
                "results: Vec<String>,\n    summary: String,",
            ),
            (
                "src/core/deploy/types.rs",
                "DeployOrchestrationResult",
                "results: Vec<String>,\n    summary: String,",
            ),
            (
                "src/core/deploy/result.rs",
                "ProjectDeployResult",
                "results: Vec<String>,\n    summary: String,",
            ),
            (
                "src/commands/status.rs",
                "UpstreamDrift",
                "ahead: usize,\n    behind: usize,",
            ),
            (
                "src/core/context/report.rs",
                "GitSnapshot",
                "ahead: usize,\n    behind: usize,",
            ),
            (
                "src/core/git/operations.rs",
                "RepoSnapshot",
                "ahead: usize,\n    behind: usize,",
            ),
        ] {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, format!("struct {name} {{\n    {fields}\n}}\n")).unwrap();
        }

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
        assert!(
            findings.is_empty(),
            "boundary data contract field overlaps should not suggest extraction: {:?}",
            findings
        );
    }

    #[test]
    fn keeps_generic_pair_signal_with_local_or_suffix_affinity() {
        let dir = tempfile::tempdir().unwrap();
        let network = dir.path().join("src/network");
        std::fs::create_dir_all(&network).unwrap();

        for name in &["PrimaryEndpoint", "ReplicaEndpoint", "FallbackEndpoint"] {
            std::fs::write(
                network.join(format!("{}.rs", name.to_lowercase())),
                format!("struct {name} {{\n    host: String,\n    port: u16,\n}}\n"),
            )
            .unwrap();
        }

        for (module, name) in &[
            ("filesystem", "FileRename"),
            ("workspace", "WorkspaceRename"),
            ("package", "PackageRename"),
        ] {
            let module_dir = dir.path().join("src").join(module);
            std::fs::create_dir_all(&module_dir).unwrap();
            std::fs::write(
                module_dir.join("rename.rs"),
                format!("struct {name} {{\n    from: String,\n    to: String,\n}}\n"),
            )
            .unwrap();
        }

        let descriptions: Vec<String> = detect_repeated_field_patterns(dir.path(), &test_scan())
            .into_iter()
            .map(|finding| finding.description)
            .collect();

        assert!(
            descriptions
                .iter()
                .any(|description| description.contains("[host, port]")),
            "generic pairs inside one module should still report: {:?}",
            descriptions
        );
        assert!(
            descriptions
                .iter()
                .any(|description| description.contains("[from, to]")),
            "generic pairs on structs with a shared suffix should still report: {:?}",
            descriptions
        );
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

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
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

        let findings = detect_repeated_field_patterns(dir.path(), &test_scan());
        assert!(
            findings.is_empty(),
            "Two occurrences should be below threshold"
        );
    }
}
