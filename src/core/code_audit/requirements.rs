//! Plugin requirement + bootstrap parser — collects symbols that are guaranteed
//! to exist at runtime so downstream detectors (e.g. `dead_guard`) can skip
//! `function_exists` / `class_exists` / `defined` guards on them.
//!
//! Sources of "guaranteed available" symbols:
//! 1. The WordPress plugin header's `Requires at least: X.Y` value, mapped
//!    against a hard-coded WP-core symbol-availability table.
//! 2. `composer.json` `require` and `require-dev` entries — when a known
//!    vendor package is present, its public symbols are considered available.
//! 3. Unconditional `require` / `require_once` calls from the plugin main
//!    file — anything pulled in at bootstrap is guaranteed loaded.
//!
//! The parser is lenient: every source is optional and a missing / malformed
//! file yields an empty contribution rather than an error.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
pub fn known_available_symbols(root: &Path) -> KnownSymbols {
    let mut symbols = KnownSymbols::default();

    let main_file = find_plugin_main_file(root);
    let wp_baseline = main_file
        .as_ref()
        .and_then(|p| parse_wp_requires_at_least(p))
        .unwrap_or(0);

    seed_wp_core_symbols(&mut symbols, wp_baseline);

    if let Some(ref main) = main_file {
        let required_paths = parse_bootstrap_requires(main, root);
        for path in &required_paths {
            seed_vendor_symbols_from_path(&mut symbols, path);
        }
    }

    apply_composer_requires(&mut symbols, root);

    symbols
}

/// Locate the plugin main file: a `*.php` file in `root` whose content contains
/// `Plugin Name:` in the header comment.
pub fn find_plugin_main_file(root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("php") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Check first ~50 lines for "Plugin Name:" marker.
        if content
            .lines()
            .take(50)
            .any(|l| l.contains("Plugin Name:"))
        {
            return Some(path);
        }
    }
    None
}

/// Parse `Requires at least: X.Y` from the plugin header. Returns the WP core
/// version encoded as `major * 100 + minor` (e.g. 5.3 → 503) for easy
/// comparison, or `None` if absent.
pub fn parse_wp_requires_at_least(main_file: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(main_file).ok()?;
    for line in content.lines().take(80) {
        if let Some(rest) = line.split_once("Requires at least:") {
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

/// Seed a minimum set of WP-core symbols known to exist when the plugin
/// declares a floor of WP version `wp_baseline` (encoded as major*100+minor).
///
/// The table is intentionally small and conservative — the intent is to catch
/// guards that are obviously dead given the plugin's declared floor, not to
/// enumerate every WP symbol.
fn seed_wp_core_symbols(symbols: &mut KnownSymbols, wp_baseline: u32) {
    // (symbol_name, introduced_in_encoded_version, kind)
    // kind: 'f' = function, 'c' = class, 'k' = constant
    const WP_SYMBOLS: &[(&str, u32, char)] = &[
        // Functions
        ("wp_generate_uuid4", 407, 'f'),
        ("wp_timezone", 503, 'f'),
        ("wp_timezone_string", 501, 'f'),
        ("wp_get_environment_type", 505, 'f'),
        ("wp_date", 503, 'f'),
        ("wp_json_encode", 404, 'f'),
        ("get_post_type_object", 300, 'f'),
        ("register_rest_route", 404, 'f'),
        ("register_block_type", 500, 'f'),
        ("has_blocks", 500, 'f'),
        ("parse_blocks", 500, 'f'),
        // Classes
        ("WP_Ability", 609, 'c'),
        ("WP_REST_Server", 404, 'c'),
        ("WP_REST_Request", 404, 'c'),
        ("WP_REST_Response", 404, 'c'),
        ("WP_Block_Type_Registry", 500, 'c'),
        ("WP_Block", 501, 'c'),
        ("WP_HTML_Tag_Processor", 602, 'c'),
        // Constants
        ("REST_REQUEST", 404, 'k'),
    ];

    if wp_baseline == 0 {
        return;
    }

    for (name, introduced, kind) in WP_SYMBOLS {
        if *introduced <= wp_baseline {
            match kind {
                'f' => {
                    symbols.functions.insert((*name).to_string());
                }
                'c' => {
                    symbols.classes.insert((*name).to_string());
                }
                'k' => {
                    symbols.constants.insert((*name).to_string());
                }
                _ => {}
            }
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

/// Inspect a bootstrap `require`d path and seed vendor-specific symbols when
/// the path matches a known library bootstrap.
fn seed_vendor_symbols_from_path(symbols: &mut KnownSymbols, path: &Path) {
    let p = path.to_string_lossy().replace('\\', "/");

    // Action Scheduler — loaded by `vendor/woocommerce/action-scheduler/action-scheduler.php`.
    if p.contains("action-scheduler/action-scheduler.php")
        || p.ends_with("/action-scheduler.php")
    {
        seed_action_scheduler_symbols(symbols);
    }
}

fn seed_action_scheduler_symbols(symbols: &mut KnownSymbols) {
    const AS_FUNCTIONS: &[&str] = &[
        "as_schedule_single_action",
        "as_schedule_recurring_action",
        "as_schedule_cron_action",
        "as_enqueue_async_action",
        "as_unschedule_action",
        "as_unschedule_all_actions",
        "as_next_scheduled_action",
        "as_has_scheduled_action",
        "as_get_scheduled_actions",
    ];
    const AS_CLASSES: &[&str] = &[
        "ActionScheduler",
        "ActionScheduler_Action",
        "ActionScheduler_Store",
        "ActionScheduler_Versions",
    ];
    for f in AS_FUNCTIONS {
        symbols.functions.insert((*f).to_string());
    }
    for c in AS_CLASSES {
        symbols.classes.insert((*c).to_string());
    }
}

/// Inspect `composer.json` and seed symbols for well-known packages.
fn apply_composer_requires(symbols: &mut KnownSymbols, root: &Path) {
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

    if packages.contains("woocommerce/action-scheduler") {
        seed_action_scheduler_symbols(symbols);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_plugin_main(dir: &Path, requires_at_least: Option<&str>, body: &str) -> PathBuf {
        let header_line = requires_at_least
            .map(|v| format!(" * Requires at least: {}\n", v))
            .unwrap_or_default();
        let content = format!(
            "<?php\n/**\n * Plugin Name: Test Plugin\n{} */\n\n{}",
            header_line, body
        );
        let path = dir.join("plugin.php");
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parses_requires_at_least() {
        let tmp = tempfile::tempdir().unwrap();
        let main = write_plugin_main(tmp.path(), Some("6.9"), "");
        let baseline = parse_wp_requires_at_least(&main).unwrap();
        assert_eq!(baseline, 609);
    }

    #[test]
    fn seeds_wp_core_symbols_up_to_baseline() {
        let mut syms = KnownSymbols::default();
        seed_wp_core_symbols(&mut syms, 609);
        assert!(syms.has_class("WP_Ability"));
        assert!(syms.has_function("wp_timezone"));
        assert!(syms.has_function("wp_generate_uuid4"));
    }

    #[test]
    fn does_not_seed_symbols_introduced_later_than_baseline() {
        let mut syms = KnownSymbols::default();
        seed_wp_core_symbols(&mut syms, 500);
        assert!(!syms.has_class("WP_Ability"));
        assert!(syms.has_function("wp_generate_uuid4"));
    }

    #[test]
    fn missing_plugin_main_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let syms = known_available_symbols(tmp.path());
        assert!(syms.functions.is_empty());
        assert!(syms.classes.is_empty());
    }

    #[test]
    fn detects_action_scheduler_bootstrap_require() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("vendor/woocommerce/action-scheduler")).unwrap();
        fs::write(
            tmp.path()
                .join("vendor/woocommerce/action-scheduler/action-scheduler.php"),
            "<?php\n",
        )
        .unwrap();

        let main = write_plugin_main(
            tmp.path(),
            Some("6.0"),
            "require_once __DIR__ . '/vendor/woocommerce/action-scheduler/action-scheduler.php';\n",
        );
        let requires = parse_bootstrap_requires(&main, tmp.path());
        assert_eq!(requires.len(), 1);

        let syms = known_available_symbols(tmp.path());
        assert!(syms.has_function("as_schedule_single_action"));
        assert!(syms.has_function("as_unschedule_action"));
    }

    #[test]
    fn composer_require_seeds_action_scheduler() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("composer.json"),
            r#"{"require":{"woocommerce/action-scheduler":"^3.0"}}"#,
        )
        .unwrap();
        let mut syms = KnownSymbols::default();
        apply_composer_requires(&mut syms, tmp.path());
        assert!(syms.has_function("as_schedule_single_action"));
    }

    #[test]
    fn guarded_require_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let main = write_plugin_main(
            tmp.path(),
            Some("6.0"),
            "if ( ! class_exists( 'ActionScheduler' ) ) {\n    require_once __DIR__ . '/vendor/woocommerce/action-scheduler/action-scheduler.php';\n}\n",
        );
        let requires = parse_bootstrap_requires(&main, tmp.path());
        assert!(
            requires.is_empty(),
            "guarded require should be skipped, got: {:?}",
            requires
        );
    }
}
