//! source_directory_detection — extracted from codebase_map.rs.

use std::fs;
use std::path::Path;
use crate::{component, extension, Error};
use std::collections::HashMap;
use serde::Serialize;
use crate::code_audit::fingerprint::{self, FileFingerprint};
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};


pub(crate) fn default_source_extensions() -> Vec<String> {
    vec![
        "php".to_string(),
        "rs".to_string(),
        "js".to_string(),
        "ts".to_string(),
        "jsx".to_string(),
        "tsx".to_string(),
        "py".to_string(),
        "go".to_string(),
        "java".to_string(),
        "rb".to_string(),
        "swift".to_string(),
        "kt".to_string(),
    ]
}

pub(crate) fn find_source_directories(source_path: &Path) -> Vec<String> {
    let mut dirs = Vec::new();
    let source_dir_names = [
        "src",
        "lib",
        "inc",
        "app",
        "components",
        "extensions",
        "crates",
    ];

    for dir_name in &source_dir_names {
        let dir_path = source_path.join(dir_name);
        if dir_path.is_dir() {
            dirs.push(dir_name.to_string());
            if let Ok(entries) = fs::read_dir(&dir_path) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !name.starts_with('.') {
                            dirs.push(format!("{}/{}", dir_name, name));
                        }
                    }
                }
            }
        }
    }

    dirs.sort();
    dirs
}

pub(crate) fn find_source_directories_by_extension(source_path: &Path, extensions: &[String]) -> Vec<String> {
    let mut dirs = Vec::new();

    if directory_contains_source_files(source_path, extensions) {
        dirs.push(".".to_string());
    }

    if let Ok(entries) = fs::read_dir(source_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if name.starts_with('.')
                || name == "node_modules"
                || name == "vendor"
                || name == "docs"
                || name == "tests"
                || name == "test"
                || name == "__pycache__"
                || name == "target"
                || name == "build"
                || name == "dist"
            {
                continue;
            }

            if path.is_dir() && directory_contains_source_files(&path, extensions) {
                dirs.push(name.clone());

                if let Ok(sub_entries) = fs::read_dir(&path) {
                    for sub_entry in sub_entries.flatten() {
                        let sub_path = sub_entry.path();
                        let sub_name = sub_entry.file_name().to_string_lossy().to_string();

                        if !sub_name.starts_with('.')
                            && sub_path.is_dir()
                            && directory_contains_source_files(&sub_path, extensions)
                        {
                            dirs.push(format!("{}/{}", name, sub_name));
                        }
                    }
                }
            }
        }
    }

    dirs.sort();
    dirs
}

pub(crate) fn directory_contains_source_files(dir: &Path, extensions: &[String]) -> bool {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    let ext_str = ext.to_string_lossy().to_lowercase();
                    if extensions.iter().any(|e| e.to_lowercase() == ext_str) {
                        return true;
                    }
                }
            }
        }
    }
    false
}
