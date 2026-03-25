//! constants — extracted from operations.rs.

use serde::{Deserialize, Serialize};
use std::process::Command;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};


pub(crate) const DEFAULT_COMMIT_LIMIT: usize = 10;

pub(crate) const VERBOSE_UNTRACKED_THRESHOLD: usize = 200;

pub(crate) const NOISY_UNTRACKED_DIRS: [&str; 8] = [
