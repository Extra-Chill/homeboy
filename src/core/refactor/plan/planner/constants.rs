//! constants — extracted from planner.rs.

use crate::code_audit::CodeAuditResult;
use crate::component::Component;
use crate::engine::run_dir::{self, RunDir};
use crate::engine::undo::UndoSnapshot;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use crate::Error;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use super::super::verify::AuditConvergenceScoring;


pub const KNOWN_PLAN_SOURCES: &[&str] = &["audit", "lint", "test"];

/// Name of the env var pointing to previous command output files.
///
/// When set, `--from audit` reads the cached audit result instead of
/// re-running the audit. The action sets this during `run-homeboy-commands.sh`
/// and it persists across steps via `GITHUB_ENV`.
pub(crate) const OUTPUT_DIR_ENV: &str = "HOMEBOY_OUTPUT_DIR";
