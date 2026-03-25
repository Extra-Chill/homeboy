//! collect_source_files — extracted from test.rs.

use std::path::{Path, PathBuf};
use crate::error::{Error, Result};
use std::collections::HashSet;
use regex::Regex;
use serde::Serialize;
use super::ScaffoldConfig;
use super::ScaffoldBatchResult;
use super::scaffold_file;


pub fn scaffold_untested(
    root: &Path,
    config: &ScaffoldConfig,
    write: bool,
) -> Result<ScaffoldBatchResult> {
    let source_dirs = if config.language == "rust" {
        vec!["src"]
    } else {
        vec!["src", "inc", "lib"]
    };

    let ext = if config.language == "rust" {
        "rs"
    } else {
        "php"
    };

    let mut source_files = Vec::new();
    for dir in &source_dirs {
        let dir_path = root.join(dir);
        if dir_path.exists() {
            source_files.extend(collect_source_files(&dir_path, ext));
        }
    }

    let mut results = Vec::new();
    let mut total_stubs = 0;
    let mut total_written = 0;
    let mut total_skipped = 0;

    for source_file in &source_files {
        let result = scaffold_file(source_file, root, config, write)?;
        if result.skipped {
            total_skipped += 1;
        } else {
            total_stubs += result.stub_count;
            if result.written {
                total_written += 1;
            }
        }
        results.push(result);
    }

    Ok(ScaffoldBatchResult {
        results,
        total_stubs,
        total_written,
        total_skipped,
    })
}

pub(crate) fn collect_source_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

    let config = ScanConfig {
        extensions: ExtensionFilter::Only(vec![ext.to_string()]),
        ..Default::default()
    };
    codebase_scan::walk_files(dir, &config)
}
