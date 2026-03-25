//! audit — extracted from refactor.rs.

use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use homeboy::refactor::{
    self, auto, AddResult, MoveResult, RenameContext, RenameScope, RenameSpec, RenameTargeting,
};
use crate::commands::CmdResult;
use clap::{Args, Subcommand};
use homeboy::engine::execution_context::{self, ResolveOptions};
use serde::Serialize;
use std::collections::HashSet;
use super::super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs};
use super::RefactorOutput;


pub(crate) fn parse_audit_findings(values: &[String]) -> homeboy::Result<Vec<AuditFinding>> {
    values
        .iter()
        .map(|value| {
            value.parse::<AuditFinding>().map_err(|_| {
                homeboy::Error::validation_invalid_argument(
                    "kind",
                    format!("Unknown audit finding kind: {}", value),
                    None,
                    None,
                )
            })
        })
        .collect()
}

pub(crate) fn run_add_from_audit(source: &str, write: bool) -> CmdResult<RefactorOutput> {
    // Parse audit JSON from @file, stdin (-), file path, or inline string.
    // Auto-detect bare file paths (same pattern as docs generate --from-audit).
    let effective_source = if !source.starts_with('{')
        && !source.starts_with('[')
        && source != "-"
        && !source.starts_with('@')
        && std::path::Path::new(source).exists()
    {
        format!("@{}", source)
    } else {
        source.to_string()
    };

    let json_content = crate::commands::merge_json_sources(Some(&effective_source), &[])?;

    // Parse audit result — handle both envelope and raw formats.
    // The envelope format wraps the audit in a "data" field (from `homeboy --format json audit`).
    let audit: CodeAuditResult = if let Some(data) = json_content.get("data") {
        serde_json::from_value(data.clone())
    } else {
        serde_json::from_value(json_content)
    }
    .map_err(|e| {
        homeboy::Error::validation_invalid_json(
            e,
            Some("parse audit result for refactor add".to_string()),
            Some(
                "Input must be output from `homeboy audit <component>`. \
                 Save it with: homeboy --format json audit <component> > audit.json"
                    .to_string(),
            ),
        )
    })?;

    let fix_result = refactor::fixes_from_audit(&audit, write)?;

    let exit_code = if fix_result.total_insertions > 0 {
        1
    } else {
        0
    };

    homeboy::log_status!(
        "refactor",
        "{} fix(es) across {} file(s){}",
        fix_result.total_insertions,
        fix_result.fixes.len(),
        if write {
            format!(" — {} written", fix_result.files_modified)
        } else {
            " (dry run)".to_string()
        }
    );

    Ok((
        RefactorOutput::AddFromAudit {
            source_path: audit.source_path,
            fix_result,
            dry_run: !write,
        },
        exit_code,
    ))
}
