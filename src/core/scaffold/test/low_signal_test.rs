//! low_signal_test — extracted from test.rs.

use std::collections::HashSet;
use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};
use super::MAX_AUTO_SCAFFOLD_STUBS;


pub(crate) fn is_low_signal_test_name(name: &str) -> bool {
    matches!(name, "test_run" | "test_new" | "test_validate")
}

pub(crate) fn passes_scaffold_quality_gate(test_names: &[String]) -> bool {
    if test_names.is_empty() {
        return false;
    }
    if test_names.len() > MAX_AUTO_SCAFFOLD_STUBS {
        return false;
    }

    let low_signal = test_names
        .iter()
        .filter(|name| is_low_signal_test_name(name))
        .count();
    let meaningful = test_names.len().saturating_sub(low_signal);

    if meaningful == 0 {
        return false;
    }

    if test_names.len() >= 3 && low_signal > meaningful {
        return false;
    }

    true
}
