//! file — extracted from test.rs.

use std::path::{Path, PathBuf};
use crate::engine::local_files;
use crate::error::{Error, Result};
use std::collections::HashSet;
use regex::Regex;
use serde::Serialize;
use super::ScaffoldConfig;
use super::passes_scaffold_quality_gate;
use super::generate_php_test;
use super::generate_rust_test;
use super::generated_test_names;
use super::ScaffoldResult;


pub fn test_file_path(source_path: &Path, root: &Path) -> PathBuf {
    let relative = source_path.strip_prefix(root).unwrap_or(source_path);
    let rel_str = relative.to_string_lossy();

    if rel_str.ends_with(".php") {
        let stripped = rel_str
            .strip_prefix("src/")
            .or_else(|| rel_str.strip_prefix("inc/"))
            .or_else(|| rel_str.strip_prefix("lib/"))
            .unwrap_or(&rel_str);
        let without_ext = stripped.strip_suffix(".php").unwrap_or(stripped);
        return root.join(format!("tests/Unit/{}Test.php", without_ext));
    }

    if rel_str.ends_with(".rs") {
        let stripped = rel_str.strip_prefix("src/").unwrap_or(&rel_str);
        let without_ext = stripped.strip_suffix(".rs").unwrap_or(stripped);
        return root.join(format!("tests/{}_test.rs", without_ext));
    }

    root.join("tests").join(relative)
}

pub fn scaffold_file(
    source_path: &Path,
    root: &Path,
    config: &ScaffoldConfig,
    write: bool,
) -> Result<ScaffoldResult> {
    let relative = source_path
        .strip_prefix(root)
        .unwrap_or(source_path)
        .to_string_lossy()
        .to_string();

    let content = local_files::read_file(source_path, "read source file")?;
    let classes = if config.language == "rust" {
        extract_rust(&content)
    } else {
        extract_php(&content)
    };

    let test_path = test_file_path(source_path, root);
    let test_relative = test_path
        .strip_prefix(root)
        .unwrap_or(&test_path)
        .to_string_lossy()
        .to_string();

    if test_path.exists() {
        return Ok(ScaffoldResult {
            source_file: relative,
            test_file: test_relative,
            stub_count: 0,
            content: String::new(),
            written: false,
            skipped: true,
            classes,
        });
    }

    let generated_names = generated_test_names(&classes, config);
    let stub_count = generated_names.len();

    if !passes_scaffold_quality_gate(&generated_names) {
        return Ok(ScaffoldResult {
            source_file: relative,
            test_file: test_relative,
            stub_count: 0,
            content: String::new(),
            written: false,
            skipped: false,
            classes,
        });
    }

    let generated = if config.language == "rust" {
        generate_rust_test(&classes, config)
    } else {
        generate_php_test(&classes, config)
    };

    if write && !generated.is_empty() {
        if let Some(parent) = test_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(
                    format!("Failed to create test directory: {}", e),
                    Some("scaffold.write".to_string()),
                )
            })?;
        }
        local_files::write_file(&test_path, &generated, "write test scaffold")?;
    }

    Ok(ScaffoldResult {
        source_file: relative,
        test_file: test_relative,
        stub_count,
        content: generated,
        written: write,
        skipped: false,
        classes,
    })
}
