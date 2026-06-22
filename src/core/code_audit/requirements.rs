//! Runtime requirement + bootstrap parser — collects symbols that are guaranteed
//! to exist at runtime so downstream detectors (e.g. `dead_guard`) can skip
//! runtime-availability guards on them.
//!
//! Core stays ecosystem-agnostic: every file name, statement keyword, and guard
//! marker used here is supplied by the owning language extension via
//! `AuditConfig.known_symbols`. When an extension declares no source-scan or
//! manifest-package contract, the corresponding step is skipped entirely.
//!
//! Sources of "guaranteed available" symbols:
//! 1. Extension-provided header-version rules mapped against symbols.
//! 2. Extension-declared dependency manifests (file + package keys) mapped
//!    against extension-provided package rules.
//! 3. Unconditional include/require statements (keywords + guard markers
//!    provided by the extension) from entry files mapped against
//!    extension-provided bootstrap-path rules.
//!
//! The parser is lenient: every source is optional and a missing / malformed
//! file yields an empty contribution rather than an error.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::core::component::audit::KnownSymbolSourceScanConfig;
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
        // Case-insensitive lookup: type/class identifiers in several ecosystems
        // are case-insensitive, so normalize before comparing.
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
    let entry_file_extensions = entry_file_extensions(audit_config);

    for provider in &providers.header_versions {
        if let Some(main_file) =
            find_file_with_marker(root, &provider.file_marker, &entry_file_extensions)
        {
            seed_header_version_symbols(&mut symbols, &main_file, provider);
        }
    }

    if let Some(scan) = providers.source_scan.as_ref() {
        for main in find_bootstrap_files(root, audit_config, &entry_file_extensions) {
            let required_paths = parse_bootstrap_requires(&main, root, scan);
            for path in &required_paths {
                seed_symbols_from_bootstrap_path(&mut symbols, path, audit_config);
            }
        }
    }

    apply_manifest_requires(&mut symbols, root, audit_config);

    symbols
}

/// Entry-file extensions declared by the owning extension's source-scan
/// contract. Empty when no extension configures source scanning.
fn entry_file_extensions(audit_config: &AuditConfig) -> Vec<String> {
    audit_config
        .known_symbols
        .source_scan
        .as_ref()
        .map(|scan| scan.entry_file_extensions.clone())
        .unwrap_or_default()
}

fn find_bootstrap_files(
    root: &Path,
    audit_config: &AuditConfig,
    entry_file_extensions: &[String],
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for provider in &audit_config.known_symbols.header_versions {
        if let Some(path) =
            find_file_with_marker(root, &provider.file_marker, entry_file_extensions)
        {
            if !files.contains(&path) {
                files.push(path);
            }
        }
    }
    files
}

/// Locate a root-level entry file whose header contains an extension-owned
/// marker. Only files matching an extension-declared entry-file extension are
/// considered; with no declared extensions, nothing matches.
pub fn find_file_with_marker(
    root: &Path,
    marker: &str,
    entry_file_extensions: &[String],
) -> Option<PathBuf> {
    if entry_file_extensions.is_empty() {
        return None;
    }
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if !ext.is_some_and(|ext| entry_file_extensions.iter().any(|allowed| allowed == ext)) {
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

/// Parse unconditional include/require statements from an entry file and return
/// resolved absolute paths that live under `root`.
///
/// The statement keywords (`scan.require_keywords`) and guard markers
/// (`scan.guard_markers`) are supplied by the owning extension so core does not
/// hardcode any single language's syntax. "Unconditional" means: not inside an
/// `if (...) {` block whose opening line mentions one of the configured guard
/// markers.
pub fn parse_bootstrap_requires(
    main_file: &Path,
    root: &Path,
    scan: &KnownSymbolSourceScanConfig,
) -> Vec<PathBuf> {
    if scan.require_keywords.is_empty() {
        return Vec::new();
    }

    let Ok(content) = std::fs::read_to_string(main_file) else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    let main_dir = main_file.parent().unwrap_or(root);

    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let requires_kind = scan
            .require_keywords
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
                && scan
                    .guard_markers
                    .iter()
                    .any(|marker| prev.contains(marker.as_str()))
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

/// Inspect extension-declared dependency manifests and seed symbols for
/// extension-provided packages. The manifest file name and the package-holding
/// keys both come from the provider, so core does not assume any one ecosystem.
fn apply_manifest_requires(symbols: &mut KnownSymbols, root: &Path, config: &AuditConfig) {
    use std::collections::HashMap;

    // Cache parsed manifests so multiple providers naming the same file only
    // read/parse it once.
    let mut manifest_packages: HashMap<String, HashSet<String>> = HashMap::new();

    for provider in &config.known_symbols.manifest_packages {
        let packages = manifest_packages
            .entry(provider.manifest_file.clone())
            .or_insert_with(|| {
                read_manifest_packages(root, &provider.manifest_file, &provider.package_keys)
            });
        if packages.contains(&provider.package) {
            seed_entries(symbols, &provider.symbols);
        }
    }
}

/// Read a JSON dependency manifest and collect every declared package name found
/// under the provided package-holding keys.
fn read_manifest_packages(
    root: &Path,
    manifest_file: &str,
    package_keys: &[String],
) -> HashSet<String> {
    let mut packages: HashSet<String> = HashSet::new();
    let Ok(content) = std::fs::read_to_string(root.join(manifest_file)) else {
        return packages;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return packages;
    };

    for key in package_keys {
        if let Some(obj) = json.get(key).and_then(|v| v.as_object()) {
            for name in obj.keys() {
                packages.insert(name.to_string());
            }
        }
    }

    packages
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
                "manifest_packages": [
                    {
                        "manifest_file": "deps.json",
                        "package_keys": ["require", "require-dev"],
                        "package": "vendor/runtime-queue",
                        "symbols": [
                            {"name": "runtime_schedule_once", "kind": "function"},
                            {"name": "RuntimeScheduler", "kind": "class"}
                        ]
                    }
                ],
                "bootstrap_paths": [
                    {
                        "path_contains": "runtime-queue/runtime-queue.inc",
                        "symbols": [
                            {"name": "runtime_schedule_once", "kind": "function"},
                            {"name": "RuntimeScheduler", "kind": "class"}
                        ]
                    }
                ],
                "source_scan": {
                    "entry_file_extensions": ["inc"],
                    "require_keywords": ["require_once", "require", "include_once", "include"],
                    "guard_markers": ["class_exists", "function_exists", "defined"]
                }
            }
        }))
        .unwrap()
    }

    fn test_scan() -> KnownSymbolSourceScanConfig {
        test_config()
            .known_symbols
            .source_scan
            .expect("test config declares source_scan")
    }

    fn write_runtime_main(dir: &Path, requires_at_least: Option<&str>, body: &str) -> PathBuf {
        let header_line = requires_at_least
            .map(|v| format!(" * Runtime Requires: {}\n", v))
            .unwrap_or_default();
        let content = format!(
            "<?\n/**\n * Runtime Plugin: Test Plugin\n{} */\n\n{}",
            header_line, body
        );
        let path = dir.join("plugin.inc");
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
            tmp.path().join("vendor/runtime-queue/runtime-queue.inc"),
            "<?\n",
        )
        .unwrap();

        let main = write_runtime_main(
            tmp.path(),
            Some("2.0"),
            "require_once __DIR__ . '/vendor/runtime-queue/runtime-queue.inc';\n",
        );
        let requires = parse_bootstrap_requires(&main, tmp.path(), &test_scan());
        assert_eq!(requires.len(), 1);

        let syms = known_available_symbols(tmp.path(), &test_config());
        assert!(syms.has_function("runtime_schedule_once"));
        assert!(syms.has_class("RuntimeScheduler"));
    }

    #[test]
    fn manifest_require_seeds_configured_package() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("deps.json"),
            r#"{"require":{"vendor/runtime-queue":"^3.0"}}"#,
        )
        .unwrap();
        let mut syms = KnownSymbols::default();
        apply_manifest_requires(&mut syms, tmp.path(), &test_config());
        assert!(syms.has_function("runtime_schedule_once"));
    }

    #[test]
    fn skips_source_scan_when_unconfigured() {
        // With no source_scan contract, core performs no entry-file scanning.
        let tmp = tempfile::tempdir().unwrap();
        let main = write_runtime_main(
            tmp.path(),
            Some("2.0"),
            "require_once __DIR__ . '/vendor/runtime-queue/runtime-queue.inc';\n",
        );
        let empty = KnownSymbolSourceScanConfig::default();
        assert!(parse_bootstrap_requires(&main, tmp.path(), &empty).is_empty());
    }

    #[test]
    fn guarded_require_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let main = write_runtime_main(
            tmp.path(),
            Some("2.0"),
            "if ( ! class_exists( 'RuntimeScheduler' ) ) {\n    require_once __DIR__ . '/vendor/runtime-queue/runtime-queue.inc';\n}\n",
        );
        let requires = parse_bootstrap_requires(&main, tmp.path(), &test_scan());
        assert!(
            requires.is_empty(),
            "guarded require should be skipped, got: {:?}",
            requires
        );
    }
}
