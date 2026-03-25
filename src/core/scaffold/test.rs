//! Test scaffold — generate test stubs from source file conventions.
//!
//! Reads a source file, extracts its public API (methods, functions),
//! and generates a test file with one stub per public method. The output
//! follows project conventions for test file naming, base classes, and
//! assertion style.
//!
//! Supports two extraction modes:
//! - Grammar-based: uses extension-provided grammar.toml (preferred)
//! - Legacy regex: hardcoded patterns as fallback

mod collect_source_files;
mod extract_php;
mod extract_rust;
mod file;
mod grammar;
mod low_signal_test;
mod scaffold_config;
mod snake_case;
mod types;

pub use collect_source_files::*;
pub use extract_php::*;
pub use extract_rust::*;
pub use file::*;
pub use grammar::*;
pub use low_signal_test::*;
pub use scaffold_config::*;
pub use snake_case::*;
pub use types::*;


use std::collections::HashSet;

use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::engine::local_files;
use crate::error::{Error, Result};

impl ScaffoldConfig {
    pub fn php() -> Self {
        Self {
            base_class: "WP_UnitTestCase".to_string(),
            base_class_import: "WP_UnitTestCase".to_string(),
            test_prefix: "test_".to_string(),
            incomplete_body: "$this->markTestIncomplete('TODO: implement');".to_string(),
            language: "php".to_string(),
        }
    }

    pub fn rust() -> Self {
        Self {
            base_class: String::new(),
            base_class_import: String::new(),
            test_prefix: "test_".to_string(),
            incomplete_body: "todo!(\"implement test\");".to_string(),
            language: "rust".to_string(),
        }
    }
}
