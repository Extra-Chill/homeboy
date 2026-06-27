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

use crate::core::component::DetectorProfileConfig;
use crate::core::engine::codebase_scan::CodebaseSnapshot;

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
#[allow(unused_imports)]
use super::fingerprint::FileFingerprint;

/// Minimum number of structs sharing a field group to report.
const MIN_OCCURRENCES: usize = 3;

/// Minimum number of fields in a group to report.
const MIN_GROUP_SIZE: usize = 2;

/// Resolve the file-extension scan tokens this detector would walk for a given
/// profile. Used by the audit pipeline to ensure the shared source snapshot is a
/// superset of the detector's inputs, so filtering the snapshot here reproduces
/// the detector's previous independent walk exactly.
pub(in crate::core::code_audit) fn scan_token_extensions(
    config: &DetectorProfileConfig,
) -> Vec<String> {
    ResolvedFieldScan::from_config(config).scan_tokens
}

/// Run repeated-field-pattern detection over the shared audit source snapshot.
///
/// Consumes the in-memory `(path, content)` view built once during discovery
/// rather than re-walking and re-reading the tree. The shared snapshot is a
/// superset of the extensions this detector scans, so filtering by the resolved
/// scan tokens reproduces the previous per-file input set (in walk order),
/// keeping findings identical.
pub(in crate::core::code_audit) fn run(
    snapshot: &CodebaseSnapshot,
    config: &DetectorProfileConfig,
) -> Vec<Finding> {
    let settings = ResolvedFieldScan::from_config(config);
    if settings.scan_tokens.is_empty() {
        // No scan tokens resolved (builtin defaults disabled and none declared):
        // core ships no built-in ecosystem scan list inside the detector, so the
        // detector stays inert until a component declares its languages.
        return Vec::new();
    }
    detect_repeated_field_patterns(snapshot, &settings)
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

        // Inline-test-strip and test-file-suffix tokens fall back to the
        // agnostic builtin set when a component opts into defaults but has not
        // declared them. Without this, builtin-default components scan `.rs`
        // files yet never strip `#[cfg(test)]` modules, so inline test fixtures
        // leak into production field-pattern findings (#5576).
        let resolve_with_builtin = |configured: &[String], builtin: &[&str]| -> Vec<String> {
            if !configured.is_empty() {
                configured.to_vec()
            } else if config.use_builtin_defaults {
                builtin.iter().map(|token| (*token).to_string()).collect()
            } else {
                Vec::new()
            }
        };

        Self {
            scan_tokens,
            type_before_name_tokens: config.field_pattern_type_before_name_tokens.clone(),
            inline_test_strip_tokens: resolve_with_builtin(
                &config.field_pattern_inline_test_strip_tokens,
                Language::builtin_inline_test_strip_tokens(),
            ),
            test_file_suffixes: resolve_with_builtin(
                &config.test_file_suffixes,
                Language::builtin_test_file_suffixes(),
            ),
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

fn detect_repeated_field_patterns(
    snapshot: &CodebaseSnapshot,
    settings: &ResolvedFieldScan,
) -> Vec<Finding> {
    let root = snapshot.root();
    let mut all_structs: Vec<StructDef> = Vec::new();

    for (file_path, content) in snapshot.iter() {
        // Filter the shared snapshot to the detector's scan-token extensions,
        // matching the `ExtensionFilter::Only(scan_tokens)` walk this detector
        // previously performed.
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !settings.scan_tokens.iter().any(|token| token == ext) {
            continue;
        }

        let relative = match file_path.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        // Skip test files.
        if settings.is_test_path(&relative) {
            continue;
        }

        let scan_content = if settings.needs_inline_test_strip(&relative) {
            std::borrow::Cow::Owned(strip_rust_cfg_test_modules(content))
        } else {
            std::borrow::Cow::Borrowed(content)
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
mod tests;
