//! Generic codebase file walker and content search utility.
//!
//! Provides a shared infrastructure for walking source files and searching content
//! across the codebase. Used by refactor rename, code audit, docs audit, and others.
//!
//! Zero domain knowledge — all language-specific behavior comes from callers.

mod callback_based_walking;
mod constants;
mod content_search_boundary;
mod entry_walking_files;
mod file_walking;
mod high_level_search;
mod search_early_return;
mod types;

pub use callback_based_walking::*;
pub use constants::*;
pub use content_search_boundary::*;
pub use entry_walking_files::*;
pub use file_walking::*;
pub use high_level_search::*;
pub use search_early_return::*;
pub use types::*;


use std::path::{Path, PathBuf};

// ============================================================================
// Skip directory configuration
// ============================================================================

// ============================================================================
// Extension filter
// ============================================================================

// ============================================================================
// Scan configuration
// ============================================================================

// ============================================================================
// File walking
// ============================================================================

// ============================================================================
// Callback-based walking
// ============================================================================

// ============================================================================
// Search with early return
// ============================================================================

// ============================================================================
// Entry walking (files + directories)
// ============================================================================

// ============================================================================
// Shared skip-dir logic
// ============================================================================

// ============================================================================
// Content search — boundary-aware matching
// ============================================================================

// ============================================================================
// High-level search: walk files + search content
// ============================================================================

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- File walking tests ---

    #[test]
    fn walk_files_skips_always_dirs() {
        let dir = std::env::temp_dir().join("homeboy_scan_always_skip_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("node_modules"));
        let _ = std::fs::create_dir_all(dir.join("vendor"));
        let _ = std::fs::create_dir_all(dir.join("src"));

        std::fs::write(dir.join("node_modules/lib.js"), "x").unwrap();
        std::fs::write(dir.join("vendor/lib.php"), "x").unwrap();
        std::fs::write(dir.join("src/main.rs"), "x").unwrap();

        let files = walk_files(&dir, &ScanConfig::default());
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.rs".to_string()));
        assert!(!names.contains(&"lib.js".to_string()));
        assert!(!names.contains(&"lib.php".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_files_root_only_skip_at_root() {
        let dir = std::env::temp_dir().join("homeboy_scan_root_skip_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("build"));
        let _ = std::fs::create_dir_all(dir.join("scripts/build"));

        std::fs::write(dir.join("build/output.rs"), "x").unwrap();
        std::fs::write(dir.join("scripts/build/setup.sh"), "x").unwrap();

        let files = walk_files(&dir, &ScanConfig::default());
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(
            names.contains(&"setup.sh".to_string()),
            "Should find scripts/build/setup.sh"
        );
        assert!(
            !names.contains(&"output.rs".to_string()),
            "Should skip root-level build/"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_files_extension_filter_only() {
        let dir = std::env::temp_dir().join("homeboy_scan_ext_only_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("main.rs"), "x").unwrap();
        std::fs::write(dir.join("style.css"), "x").unwrap();
        std::fs::write(dir.join("readme.md"), "x").unwrap();

        let config = ScanConfig {
            extensions: ExtensionFilter::Only(vec!["rs".to_string(), "md".to_string()]),
            ..Default::default()
        };
        let files = walk_files(&dir, &config);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.rs".to_string()));
        assert!(names.contains(&"readme.md".to_string()));
        assert!(!names.contains(&"style.css".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_files_skip_hidden() {
        let dir = std::env::temp_dir().join("homeboy_scan_hidden_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join(".hidden"));

        std::fs::write(dir.join(".hidden/secret.rs"), "x").unwrap();
        std::fs::write(dir.join(".dotfile.rs"), "x").unwrap();
        std::fs::write(dir.join("visible.rs"), "x").unwrap();

        let config = ScanConfig {
            skip_hidden: true,
            ..Default::default()
        };
        let files = walk_files(&dir, &config);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"visible.rs".to_string()));
        assert!(!names.contains(&"secret.rs".to_string()));
        assert!(!names.contains(&".dotfile.rs".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_files_pycache_always_skipped() {
        let dir = std::env::temp_dir().join("homeboy_scan_pycache_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("src/__pycache__"));

        std::fs::write(dir.join("src/__pycache__/module.py"), "x").unwrap();
        std::fs::write(dir.join("src/main.py"), "x").unwrap();

        let files = walk_files(&dir, &ScanConfig::default());
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.py".to_string()));
        assert!(!names.contains(&"module.py".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Boundary matching tests ---

    #[test]
    fn boundary_matches_word_boundaries() {
        assert_eq!(find_boundary_matches("pub mod widget;", "widget"), vec![8]);
        assert_eq!(find_boundary_matches("load_widget", "widget"), vec![5]);
        assert_eq!(find_boundary_matches("WIDGET_DIR", "WIDGET"), vec![0]);
    }

    #[test]
    fn boundary_rejects_partial_match() {
        assert!(find_boundary_matches("widgetry", "widget").is_empty());
        assert!(find_boundary_matches("rewidget", "widget").is_empty());
    }

    #[test]
    fn boundary_matches_camel_case() {
        assert_eq!(find_boundary_matches("WidgetManifest", "Widget"), vec![0]);
        assert_eq!(find_boundary_matches("loadWidget", "Widget"), vec![4]);
    }

    #[test]
    fn boundary_matches_consecutive_uppercase() {
        // WPAgent → WP at 0, Agent at 2
        assert_eq!(find_boundary_matches("WPAgent", "Agent"), vec![2]);
        assert_eq!(find_boundary_matches("WPAgent", "WP"), vec![0]);
        assert_eq!(find_boundary_matches("XMLParser", "XML"), vec![0]);
        assert_eq!(find_boundary_matches("XMLParser", "Parser"), vec![3]);
    }

    // --- Literal matching tests ---

    #[test]
    fn literal_matches_exact_substring() {
        assert_eq!(find_literal_matches("the-widget-plugin", "widget"), vec![4]);
        // Literal matches inside words (no boundaries)
        assert_eq!(find_literal_matches("widgetry", "widget"), vec![0]);
    }

    // --- Case-insensitive matching tests ---

    #[test]
    fn case_insensitive_finds_different_casing() {
        let matches = find_case_insensitive_matches("namespace WPAgent;", "wpagent");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, 10); // position
        assert_eq!(matches[0].1, "WPAgent"); // actual casing preserved
    }

    #[test]
    fn case_insensitive_finds_multiple_casings() {
        let text = "WPAgent and wpagent and Wpagent";
        let matches = find_case_insensitive_matches(text, "wpagent");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].1, "WPAgent");
        assert_eq!(matches[1].1, "wpagent");
        assert_eq!(matches[2].1, "Wpagent");
    }

    // --- discover_casing tests ---

    #[test]
    fn discover_casing_finds_actual_convention() {
        let dir = std::env::temp_dir().join("homeboy_scan_discover_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.php"),
            "namespace WPAgent;\nuse WPAgent\\Tools;\nclass WPAgent_Loader {}\n",
        )
        .unwrap();

        let config = ScanConfig::default();
        let casings = discover_casing(&dir, "wpagent", &config);

        assert!(!casings.is_empty());
        // Most frequent casing should be "WPAgent"
        assert_eq!(casings[0].0, "WPAgent");
        assert_eq!(casings[0].1, 3); // 3 occurrences

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- High-level search tests ---

    #[test]
    fn search_boundary_mode() {
        let dir = std::env::temp_dir().join("homeboy_scan_search_boundary_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.rs"),
            "fn widget_init() {}\nlet widgetry = true;\n",
        )
        .unwrap();

        let config = ScanConfig::default();
        let results = search(&dir, "widget", &SearchMode::Boundary, &config);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line, 1);
        assert_eq!(results[0].matched, "widget");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_literal_mode() {
        let dir = std::env::temp_dir().join("homeboy_scan_search_literal_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.rs"),
            "fn widget_init() {}\nlet widgetry = true;\n",
        )
        .unwrap();

        let config = ScanConfig::default();
        let results = search(&dir, "widget", &SearchMode::Literal, &config);

        // Literal matches both: widget in widget_init AND widget in widgetry
        assert_eq!(results.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_case_insensitive_mode() {
        let dir = std::env::temp_dir().join("homeboy_scan_search_ci_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.php"),
            "namespace WPAgent;\nconst WP_AGENT = true;\n",
        )
        .unwrap();

        let config = ScanConfig::default();
        let results = search(&dir, "wpagent", &SearchMode::CaseInsensitive, &config);

        assert_eq!(results.len(), 1); // Only "WPAgent" matches (WP_AGENT has an underscore)
        assert_eq!(results[0].matched, "WPAgent");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
