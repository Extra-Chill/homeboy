//! types — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};


/// Check if match is inside a string literal or follows a property accessor.
pub(crate) fn is_key_context(line: &str, col: usize, match_len: usize) -> bool {
    let bytes = line.as_bytes();

    // Check if preceded by `.`, `->`, or `::` (property/method access)
    let before = &line[..col];
    let trimmed = before.trim_end();
    if trimmed.ends_with('.') || trimmed.ends_with("->") || trimmed.ends_with("::") {
        return true;
    }

    // Check if inside string quotes: count unescaped quotes before the match position.
    // If an odd number of single or double quotes precede us, we're inside a string.
    let match_end = col + match_len;
    for quote in [b'\'', b'"'] {
        let mut count = 0;
        let mut i = 0;
        while i < col {
            if bytes[i] == b'\\' {
                i += 2; // skip escaped char
                continue;
            }
            if bytes[i] == quote {
                count += 1;
            }
            i += 1;
        }
        if count % 2 == 1 {
            // Verify the closing quote is after the match
            let mut j = match_end;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == quote {
                    return true; // Inside a quoted string
                }
                j += 1;
            }
        }
    }

    false
}
