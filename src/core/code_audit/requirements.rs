//! Runtime requirement + bootstrap parser — collects symbols that are guaranteed
//! to exist at runtime so downstream detectors (e.g. `dead_guard`) can skip
//! `function_exists` / `class_exists` / `defined` guards on them.
//!
//! Sources of "guaranteed available" symbols:
//! 1. Extension-provided header-version rules mapped against symbols.
//! 2. `composer.json` `require` and `require-dev` entries mapped against
//!    extension-provided package rules.
//! 3. Unconditional `require` / `require_once` calls from entry files mapped
//!    against extension-provided bootstrap-path rules.
//!
//! The parser is lenient: every source is optional and a missing / malformed
//! file yields an empty contribution rather than an error.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::core::component::{
    AuditConfig, KnownSymbolEntry, KnownSymbolHeaderVersionProvider, KnownSymbolKind,
    KnownSymbolVersionedEntry,
};

/// Symbols guaranteed to be defined at runtime given the plugin's declared
/// requirements and its explicit bootstrap wiring.
#[derive(Debug, Default, Clone)]
pub struct KnownSymbols {
    pub functions: HashSet<String>,
    pub classes: HashSet<String>,
    pub constants: HashSet<String>,
}

impl KnownSymbols {
    pub fn has_function(&self, name: &str) -> bool {
        self.functions.contains(name)
    }

    pub fn has_class(&self, name: &str) -> bool {
        // Case-insensitive lookup: PHP class names are case-insensitive.
        let lower = name.to_ascii_lowercase();
        self.classes.iter().any(|c| c.to_ascii_lowercase() == lower)
    }

    pub fn has_constant(&self, name: &str) -> bool {
        self.constants.contains(name)
    }
}

/// Entry point: inspect a plugin root and return the set of guaranteed symbols.
pub fn known_available_symbols(root: &Path, audit_config: &AuditConfig) -> KnownSymbols {
    let mut symbols = KnownSymbols::default();
    let providers = &audit_config.known_symbols;

    for provider in &providers.header_versions {
        if let Some(main_file) = find_file_with_marker(root, &provider.file_marker) {
            seed_header_version_symbols(&mut symbols, &main_file, provider);
        }
    }

    for main in find_bootstrap_files(root, audit_config) {
        let required_paths = parse_bootstrap_requires(&main, root);
        for path in &required_paths {
            seed_symbols_from_bootstrap_path(&mut symbols, path, audit_config);
        }
    }

    apply_composer_requires(&mut symbols, root, audit_config);

    symbols
}

fn find_bootstrap_files(root: &Path, audit_config: &AuditConfig) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for provider in &audit_config.known_symbols.header_versions {
        if let Some(path) = find_file_with_marker(root, &provider.file_marker) {
            if !files.contains(&path) {
                files.push(path);
            }
        }
    }
    files
}

/// Locate a root-level PHP entry file whose header contains an extension-owned marker.
pub fn find_file_with_marker(root: &Path, marker: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("php") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if content.lines().take(80).any(|l| l.contains(marker)) {
            return Some(path);
        }
    }
    None
}

pub fn parse_header_version(main_file: &Path, header: &str) -> Option<u32> {
    let content = std::fs::read_to_string(main_file).ok()?;
    for line in content.lines().take(80) {
        if let Some(rest) = line.split_once(header) {
            let value = rest.1.trim().trim_end_matches('*').trim();
            return parse_version_encoded(value);
        }
    }
    None
}

fn parse_version_encoded(v: &str) -> Option<u32> {
    let mut parts = v.split('.');
    let major: u32 = parts.next()?.trim().parse().ok()?;
    let minor: u32 = parts
        .next()
        .and_then(|m| m.trim().parse().ok())
        .unwrap_or(0);
    Some(major * 100 + minor)
}

fn seed_header_version_symbols(
    symbols: &mut KnownSymbols,
    main_file: &Path,
    provider: &KnownSymbolHeaderVersionProvider,
) {
    let Some(baseline) = parse_header_version(main_file, &provider.version_header) else {
        return;
    };
    for entry in &provider.symbols {
        if versioned_entry_is_available(entry, baseline) {
            insert_symbol(symbols, &entry.name, &entry.kind);
        }
    }
}

fn versioned_entry_is_available(entry: &KnownSymbolVersionedEntry, baseline: u32) -> bool {
    parse_version_encoded(&entry.introduced).is_some_and(|introduced| introduced <= baseline)
}

fn insert_symbol(symbols: &mut KnownSymbols, name: &str, kind: &KnownSymbolKind) {
    match kind {
        KnownSymbolKind::Function => {
            symbols.functions.insert(name.to_string());
        }
        KnownSymbolKind::Class => {
            symbols.classes.insert(name.to_string());
        }
        KnownSymbolKind::Constant => {
            symbols.constants.insert(name.to_string());
        }
    }
}

/// Parse unconditional `require` / `require_once` / `include` / `include_once`
/// calls from the plugin main file and return resolved absolute paths that
/// live under `root`.
///
/// "Unconditional" means: not inside an `if (!class_exists(...))` or similar
/// guard. We use a simple heuristic: the `require` is skipped if the previous
/// non-blank line opens a guard block (`if (...) {` mentioning `class_exists`,
/// `function_exists`, or `defined`).
pub fn parse_bootstrap_requires(main_file: &Path, root: &Path) -> Vec<PathBuf> {
    let Ok(content) = std::fs::read_to_string(main_file) else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    let main_dir = main_file.parent().unwrap_or(root);

    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let requires_kind = ["require_once", "require", "include_once", "include"]
            .iter()
            .find(|k| trimmed.starts_with(*k) && !is_identifier_continuation(trimmed, k.len()));
        if requires_kind.is_none() {
            continue;
        }

        // Skip if the previous non-blank line opens a guard block.
        let mut guarded = false;
        for j in (0..i).rev() {
            let prev = lines[j].trim();
            if prev.is_empty() {
                continue;
            }
            if prev.ends_with('{')
                && (prev.contains("if ") || prev.contains("if("))
                && (prev.contains("class_exists")
                    || prev.contains("function_exists")
                    || prev.contains("defined"))
            {
                guarded = true;
            }
            break;
        }
        if guarded {
            continue;
        }

        if let Some(path_str) = extract_require_path(trimmed) {
            let resolved = resolve_require_path(&path_str, main_dir);
            if let Some(p) = resolved {
                paths.push(p);
            }
        }
    }

    paths
}

fn is_identifier_continuation(line: &str, offset: usize) -> bool {
    line.as_bytes()
        .get(offset)
        .map(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .unwrap_or(false)
}

/// Extract a quoted path from a `require[_once] ...;` statement. Returns the
/// path string as-is (caller resolves `__DIR__ .` prefixes by stripping the
/// leading `/`).
fn extract_require_path(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' || c == b'"' {
            let quote = c;
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != quote {
                end += 1;
            }
            if end <= bytes.len() {
                let raw = &line[start..end];
                return Some(raw.to_string());
            }
        }
        i += 1;
    }
    None
}

fn resolve_require_path(raw: &str, main_dir: &Path) -> Option<PathBuf> {
    let cleaned = raw.trim_start_matches('/');
    Some(main_dir.join(cleaned))
}

fn seed_symbols_from_bootstrap_path(symbols: &mut KnownSymbols, path: &Path, config: &AuditConfig) {
    let p = path.to_string_lossy().replace('\\', "/");
    for provider in &config.known_symbols.bootstrap_paths {
        if p.contains(&provider.path_contains) || p.ends_with(&provider.path_contains) {
            seed_entries(symbols, &provider.symbols);
        }
    }
}

fn seed_entries(symbols: &mut KnownSymbols, entries: &[KnownSymbolEntry]) {
    for entry in entries {
        insert_symbol(symbols, &entry.name, &entry.kind);
    }
}

/// Inspect `composer.json` and seed symbols for extension-provided packages.
fn apply_composer_requires(symbols: &mut KnownSymbols, root: &Path, config: &AuditConfig) {
    let composer = root.join("composer.json");
    let Ok(content) = std::fs::read_to_string(&composer) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return;
    };

    let mut packages: HashSet<String> = HashSet::new();
    for key in &["require", "require-dev"] {
        if let Some(obj) = json.get(*key).and_then(|v| v.as_object()) {
            for name in obj.keys() {
                packages.insert(name.to_string());
            }
        }
    }

    for provider in &config.known_symbols.composer_packages {
        if packages.contains(&provider.package) {
            seed_entries(symbols, &provider.symbols);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_config() -> AuditConfig {
        serde_json::from_value(serde_json::json!({
            "known_symbols": {
                "header_versions": [
                    {
                        "file_marker": "Runtime Plugin:",
                        "version_header": "Runtime Requires:",
                        "symbols": [
                            {"name": "runtime_uuid", "kind": "function", "introduced": "1.2"},
                            {"name": "RuntimeCapability", "kind": "class", "introduced": "2.4"},
                            {"name": "RUNTIME_REQUEST", "kind": "constant", "introduced": "1.0"}
                        ]
                    }
                ],
                "composer_packages": [
                    {
                        "package": "vendor/runtime-queue",
                        "symbols": [
                            {"name": "runtime_schedule_once", "kind": "function"},
                            {"name": "RuntimeScheduler", "kind": "class"}
                        ]
                    }
                ],
                "bootstrap_paths": [
                    {
                        "path_contains": "runtime-queue/runtime-queue.php",
                        "symbols": [
                            {"name": "runtime_schedule_once", "kind": "function"},
                            {"name": "RuntimeScheduler", "kind": "class"}
                        ]
                    }
                ]
            }
        }))
        .unwrap()
    }

    fn write_runtime_main(dir: &Path, requires_at_least: Option<&str>, body: &str) -> PathBuf {
        let header_line = requires_at_least
            .map(|v| format!(" * Runtime Requires: {}\n", v))
            .unwrap_or_default();
        let content = format!(
            "<?php\n/**\n * Runtime Plugin: Test Plugin\n{} */\n\n{}",
            header_line, body
        );
        let path = dir.join("plugin.php");
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parses_requires_at_least() {
        let tmp = tempfile::tempdir().unwrap();
        let main = write_runtime_main(tmp.path(), Some("2.4"), "");
        let baseline = parse_header_version(&main, "Runtime Requires:").unwrap();
        assert_eq!(baseline, 204);
    }

    #[test]
    fn seeds_header_version_symbols_up_to_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        write_runtime_main(tmp.path(), Some("2.4"), "");
        let syms = known_available_symbols(tmp.path(), &test_config());
        assert!(syms.has_class("RuntimeCapability"));
        assert!(syms.has_function("runtime_uuid"));
        assert!(syms.has_constant("RUNTIME_REQUEST"));
    }

    #[test]
    fn does_not_seed_symbols_introduced_later_than_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        write_runtime_main(tmp.path(), Some("1.2"), "");
        let syms = known_available_symbols(tmp.path(), &test_config());
        assert!(!syms.has_class("RuntimeCapability"));
        assert!(syms.has_function("runtime_uuid"));
    }

    #[test]
    fn missing_plugin_main_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let syms = known_available_symbols(tmp.path(), &test_config());
        assert!(syms.functions.is_empty());
        assert!(syms.classes.is_empty());
    }

    #[test]
    fn detects_configured_bootstrap_require() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("vendor/runtime-queue")).unwrap();
        fs::write(
            tmp.path().join("vendor/runtime-queue/runtime-queue.php"),
            "<?php\n",
        )
        .unwrap();

        let main = write_runtime_main(
            tmp.path(),
            Some("2.0"),
            "require_once __DIR__ . '/vendor/runtime-queue/runtime-queue.php';\n",
        );
        let requires = parse_bootstrap_requires(&main, tmp.path());
        assert_eq!(requires.len(), 1);

        let syms = known_available_symbols(tmp.path(), &test_config());
        assert!(syms.has_function("runtime_schedule_once"));
        assert!(syms.has_class("RuntimeScheduler"));
    }

    #[test]
    fn composer_require_seeds_configured_package() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("composer.json"),
            r#"{"require":{"vendor/runtime-queue":"^3.0"}}"#,
        )
        .unwrap();
        let mut syms = KnownSymbols::default();
        apply_composer_requires(&mut syms, tmp.path(), &test_config());
        assert!(syms.has_function("runtime_schedule_once"));
    }

    #[test]
    fn guarded_require_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let main = write_runtime_main(
            tmp.path(),
            Some("2.0"),
            "if ( ! class_exists( 'RuntimeScheduler' ) ) {\n    require_once __DIR__ . '/vendor/runtime-queue/runtime-queue.php';\n}\n",
        );
        let requires = parse_bootstrap_requires(&main, tmp.path());
        assert!(
            requires.is_empty(),
            "guarded require should be skipped, got: {:?}",
            requires
        );
    }
}
