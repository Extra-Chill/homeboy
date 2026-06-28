//! Thin-command-adapter boundary detection.
//!
//! Command-layer modules are expected to stay thin adapters: parse arguments,
//! construct a typed request, call a core service, and adapt the result for
//! output. Once services are extracted, command modules tend to re-accumulate
//! orchestration (process execution, persistence, runner dispatch, business
//! logic) as a convenient place to route exceptions. This detector measures the
//! *orchestration density* of each command module and flags modules whose
//! density crosses a configured threshold.
//!
//! Unlike a single-term forbidden-pattern scan, this detector judges adapter
//! thinness holistically per module: a lone configured marker may be tolerated,
//! while accumulated orchestration weight signals that real business logic has
//! leaked into the command layer and belongs in a core service.
//!
//! Core stays ecosystem-agnostic. It owns only the density primitive; every
//! marker, path scope, extension, and allowlist comes from component config.
//! With no `audit.thin_command_adapter` config the detector is inert.

use std::path::Path;

use regex::Regex;

use crate::core::component::ThinCommandAdapterConfig;
use crate::core::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::walker;

#[cfg(test)]
#[path = "../../../../tests/core/code_audit/detectors/thin_command_adapter_test.rs"]
mod thin_command_adapter_test;

/// A compiled marker group ready to scan source lines.
struct CompiledMarkerGroup {
    label: String,
    weight: u32,
    patterns: Vec<Regex>,
}

/// A single orchestration hit contributing to a module's weight.
struct OrchestrationHit {
    label: String,
    weight: u32,
}

pub(in crate::core::code_audit) fn run(
    root: &Path,
    config: &ThinCommandAdapterConfig,
) -> Vec<Finding> {
    if config.is_empty() {
        return Vec::new();
    }

    let groups = compile_marker_groups(config);
    if groups.is_empty() {
        return Vec::new();
    }

    let ignore_line_matches = compile_ignore_line_matches(config);

    let files = match walk_candidate_files(root, config) {
        Ok(files) => files,
        Err(_) => return Vec::new(),
    };

    let mut findings = Vec::new();

    for file in files {
        let Ok(relative) = file.strip_prefix(root) else {
            continue;
        };
        let normalized = relative.to_string_lossy().replace('\\', "/");

        if !is_in_scope(&normalized, config) {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };

        let hits = scan_orchestration(&content, &groups, &ignore_line_matches, config);
        let total_weight: u32 = hits.iter().map(|hit| hit.weight).sum();
        if total_weight < config.max_orchestration_weight {
            continue;
        }

        findings.push(build_finding(config, &normalized, &hits));
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn compile_marker_groups(config: &ThinCommandAdapterConfig) -> Vec<CompiledMarkerGroup> {
    config
        .orchestration_markers
        .iter()
        .filter_map(|group| {
            let patterns: Vec<Regex> = group
                .patterns
                .iter()
                .filter_map(|pattern| Regex::new(pattern).ok())
                .collect();
            if patterns.is_empty() {
                return None;
            }
            Some(CompiledMarkerGroup {
                label: group.label.clone(),
                weight: group.weight.max(1),
                patterns,
            })
        })
        .collect()
}

fn compile_ignore_line_matches(config: &ThinCommandAdapterConfig) -> Vec<Regex> {
    config
        .ignore_line_matches
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect()
}

fn walk_candidate_files(
    root: &Path,
    config: &ThinCommandAdapterConfig,
) -> std::io::Result<Vec<std::path::PathBuf>> {
    let extensions = if config.file_extensions.is_empty() {
        ExtensionFilter::All
    } else {
        ExtensionFilter::Only(config.file_extensions.clone())
    };
    let scan = ScanConfig {
        extensions,
        ..Default::default()
    };
    Ok(codebase_scan::walk_files(root, &scan))
}

fn is_in_scope(normalized: &str, config: &ThinCommandAdapterConfig) -> bool {
    let included = config
        .include_path_contains
        .iter()
        .any(|needle| normalized.contains(needle.as_str()));
    if !included {
        return false;
    }
    if config.skip_test_paths && walker::is_test_path(normalized) {
        return false;
    }
    !config
        .exclude_path_contains
        .iter()
        .any(|needle| normalized.contains(needle.as_str()))
}

fn scan_orchestration(
    content: &str,
    groups: &[CompiledMarkerGroup],
    ignore_line_matches: &[Regex],
    config: &ThinCommandAdapterConfig,
) -> Vec<OrchestrationHit> {
    let mut hits = Vec::new();

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();

        if config
            .ignore_after_line_equals
            .iter()
            .any(|marker| trimmed == marker.as_str())
        {
            break;
        }

        if trimmed.is_empty() {
            continue;
        }

        if config
            .ignore_line_prefixes
            .iter()
            .any(|prefix| trimmed.starts_with(prefix.as_str()))
        {
            continue;
        }

        if ignore_line_matches
            .iter()
            .any(|pattern| pattern.is_match(raw_line))
        {
            continue;
        }

        if config
            .allow_line_contains
            .iter()
            .any(|marker| raw_line.contains(marker.as_str()))
        {
            continue;
        }

        for group in groups {
            if group
                .patterns
                .iter()
                .any(|pattern| pattern.is_match(raw_line))
            {
                hits.push(OrchestrationHit {
                    label: group.label.clone(),
                    weight: group.weight,
                });
            }
        }
    }

    hits
}

fn build_finding(
    config: &ThinCommandAdapterConfig,
    file: &str,
    hits: &[OrchestrationHit],
) -> Finding {
    let categories = render_categories(hits);
    Finding {
        convention: config.convention.clone(),
        severity: Severity::Warning,
        file: file.to_string(),
        description: format!(
            "Command module accumulates orchestration that belongs in a core service: {categories}"
        ),
        suggestion:
            "Keep command modules to argument parsing, typed request construction, and output \
             adaptation. Move orchestration, persistence, process execution, and artifact handling \
             into a core service this module delegates to."
                .to_string(),
        kind: AuditFinding::ThinCommandAdapterViolation,
    }
}

/// Render a deterministic, count-independent summary of which orchestration
/// categories were observed. Counts, weights, and line numbers are intentionally
/// omitted so the finding fingerprint is stable across unrelated edits — the
/// baseline tracks the module, not its exact orchestration volume.
fn render_categories(hits: &[OrchestrationHit]) -> String {
    let mut labels: Vec<&str> = hits.iter().map(|hit| hit.label.as_str()).collect();
    labels.sort_unstable();
    labels.dedup();
    labels.join(", ")
}
