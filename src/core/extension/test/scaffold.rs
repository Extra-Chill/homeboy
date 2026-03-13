//! Test scaffold workflow — orchestrates test stub generation for components.
//!
//! Owns the scaffold orchestration flow: language detection, single-file vs batch mode,
//! user-facing output, and result shaping. The underlying stub generation lives in
//! `crate::scaffold`; this module provides the component-aware workflow layer.

use crate::component::Component;
use crate::scaffold::{self, ScaffoldConfig};
use serde::Serialize;
use std::path::PathBuf;

/// Summary of a scaffold run (single-file or batch).
#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldOutput {
    pub results: Vec<ScaffoldFileOutput>,
    pub total_stubs: usize,
    pub total_written: usize,
    pub total_skipped: usize,
}

/// Per-file scaffold result.
#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldFileOutput {
    pub source_file: String,
    pub test_file: String,
    pub stub_count: usize,
    pub written: bool,
    pub skipped: bool,
}

/// Result of a scaffold workflow run, ready for command-layer output assembly.
#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldWorkflowResult {
    pub component: String,
    pub output: ScaffoldOutput,
}

/// Run scaffold workflow — generate test stubs from source files.
///
/// Handles both single-file mode (`scaffold_file` provided) and batch mode (scan all
/// untested files). Language detection is automatic (Cargo.toml → Rust, else → PHP).
pub fn run_scaffold_workflow(
    component_id: &str,
    component: &Component,
    scaffold_file: Option<&str>,
    write: bool,
) -> crate::Result<ScaffoldWorkflowResult> {
    let source_path = {
        let expanded = shellexpand::tilde(&component.local_path);
        PathBuf::from(expanded.as_ref())
    };

    // Auto-detect language
    let config = if source_path.join("Cargo.toml").exists() {
        ScaffoldConfig::rust()
    } else {
        ScaffoldConfig::php()
    };

    let mode_label = if write { "write" } else { "dry-run" };

    let output = if let Some(file) = scaffold_file {
        run_single_file_scaffold(component_id, &source_path, file, &config, write, mode_label)?
    } else {
        run_batch_scaffold(component_id, &source_path, &config, write, mode_label)?
    };

    Ok(ScaffoldWorkflowResult {
        component: component_id.to_string(),
        output,
    })
}

/// Scaffold a single source file.
fn run_single_file_scaffold(
    _component_id: &str,
    source_path: &PathBuf,
    file: &str,
    config: &ScaffoldConfig,
    write: bool,
    mode_label: &str,
) -> crate::Result<ScaffoldOutput> {
    let file_path = source_path.join(file);
    crate::log_status!(
        "scaffold",
        "Scaffolding tests for {} ({})",
        file,
        mode_label
    );

    let result = scaffold::scaffold_file(&file_path, source_path, config, write)?;

    if result.skipped {
        crate::log_status!(
            "scaffold",
            "Skipped — test file already exists: {}",
            result.test_file
        );
    } else if result.stub_count == 0 {
        crate::log_status!("scaffold", "No public methods found in {}", file);
    } else {
        crate::log_status!(
            "scaffold",
            "Generated {} test stub{} → {}{}",
            result.stub_count,
            if result.stub_count == 1 { "" } else { "s" },
            result.test_file,
            if write { " (written)" } else { " (dry-run)" }
        );

        if !write {
            // Show preview
            eprintln!("---");
            for line in result.content.lines().take(40) {
                eprintln!("{}", line);
            }
            if result.content.lines().count() > 40 {
                eprintln!("... ({} more lines)", result.content.lines().count() - 40);
            }
            eprintln!("---");
        }
    }

    Ok(ScaffoldOutput {
        results: vec![ScaffoldFileOutput {
            source_file: result.source_file.clone(),
            test_file: result.test_file.clone(),
            stub_count: result.stub_count,
            written: result.written,
            skipped: result.skipped,
        }],
        total_stubs: result.stub_count,
        total_written: if result.written { 1 } else { 0 },
        total_skipped: if result.skipped { 1 } else { 0 },
    })
}

/// Scaffold all untested files in a component.
fn run_batch_scaffold(
    component_id: &str,
    source_path: &PathBuf,
    config: &ScaffoldConfig,
    write: bool,
    mode_label: &str,
) -> crate::Result<ScaffoldOutput> {
    crate::log_status!(
        "scaffold",
        "Scanning {} for untested {} files ({})",
        component_id,
        config.language,
        mode_label
    );

    let batch = scaffold::scaffold_untested(source_path, config, write)?;

    let files_needing_tests = batch
        .results
        .iter()
        .filter(|r| !r.skipped && r.stub_count > 0)
        .count();
    let already_tested = batch.total_skipped;

    crate::log_status!(
        "scaffold",
        "{} file{} need tests, {} already have tests",
        files_needing_tests,
        if files_needing_tests == 1 { "" } else { "s" },
        already_tested
    );

    if write {
        crate::log_status!(
            "scaffold",
            "Wrote {} test file{} with {} total stubs",
            batch.total_written,
            if batch.total_written == 1 { "" } else { "s" },
            batch.total_stubs
        );
    } else if files_needing_tests > 0 {
        // Show summary of what would be created
        for result in &batch.results {
            if !result.skipped && result.stub_count > 0 {
                crate::log_status!(
                    "  new",
                    "{} → {} ({} stubs)",
                    result.source_file,
                    result.test_file,
                    result.stub_count
                );
            }
        }
        crate::log_status!(
            "hint",
            "Run with --write to create test files: homeboy test {} --scaffold --write",
            component_id
        );
    }

    Ok(ScaffoldOutput {
        results: batch
            .results
            .iter()
            .map(|r| ScaffoldFileOutput {
                source_file: r.source_file.clone(),
                test_file: r.test_file.clone(),
                stub_count: r.stub_count,
                written: r.written,
                skipped: r.skipped,
            })
            .collect(),
        total_stubs: batch.total_stubs,
        total_written: batch.total_written,
        total_skipped: batch.total_skipped,
    })
}
