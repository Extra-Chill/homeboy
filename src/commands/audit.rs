use clap::Args;
use std::path::Path;

use homeboy::code_audit::{
    self, report, run_main_audit_workflow, AuditCommandOutput, AuditRunWorkflowArgs,
};
use homeboy::refactor::{AuditConvergenceScoring, AuditVerificationToggles};

use super::utils::args::{BaselineArgs, PositionalComponentArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct AuditArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    /// Only show discovered conventions (skip findings)
    #[arg(long)]
    pub conventions: bool,

    /// Generate fix stubs for outlier files (dry run by default)
    #[arg(long)]
    pub fix: bool,

    /// Apply fixes to disk (requires --fix)
    #[arg(long, requires = "fix")]
    pub write: bool,

    /// Maximum recursive autofix iterations when writing
    #[arg(long, requires = "fix", default_value_t = 3)]
    pub max_iterations: usize,

    /// Weight for warning-level findings in convergence scoring
    #[arg(long, requires = "fix", default_value_t = 3)]
    pub warning_weight: usize,

    /// Weight for info-level findings in convergence scoring
    #[arg(long, requires = "fix", default_value_t = 1)]
    pub info_weight: usize,

    /// Disable lint smoke verification during chunk verification
    #[arg(long, requires = "fix")]
    pub no_lint_smoke: bool,

    /// Disable test smoke verification during chunk verification
    #[arg(long, requires = "fix")]
    pub no_test_smoke: bool,

    /// Restrict generated fixes to these fix kinds (repeatable)
    #[arg(long = "only", value_name = "kind")]
    pub only: Vec<String>,

    /// Exclude generated fixes for these fix kinds (repeatable)
    #[arg(long = "exclude", value_name = "kind")]
    pub exclude: Vec<String>,

    /// Update baseline when findings are resolved (ratchet forward).
    #[arg(long)]
    pub ratchet: bool,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    /// Only audit files changed since a git ref (branch, tag, or SHA).
    #[arg(long)]
    pub changed_since: Option<String>,

    /// Include compact machine-readable summary for CI wrappers
    #[arg(long)]
    pub json_summary: bool,

    /// Include full generated code in --fix JSON output (omitted by default to reduce size)
    #[arg(long, requires = "fix")]
    pub preview: bool,
}

fn parse_finding_kinds(
    values: &[String],
    flag: &str,
) -> homeboy::Result<Vec<code_audit::AuditFinding>> {
    use std::str::FromStr;
    values
        .iter()
        .map(|value| {
            code_audit::AuditFinding::from_str(value)
                .map_err(|msg| homeboy::Error::validation_invalid_argument(flag, msg, None, None))
        })
        .collect()
}

pub fn run(args: AuditArgs, _global: &GlobalArgs) -> CmdResult<AuditCommandOutput> {
    let only_kinds = parse_finding_kinds(&args.only, "only")?;
    let exclude_kinds = parse_finding_kinds(&args.exclude, "exclude")?;

    // Resolve component ID and source path
    let (resolved_id, resolved_path) = if Path::new(&args.comp.component).is_dir() {
        let effective = args
            .comp
            .path
            .as_deref()
            .unwrap_or(&args.comp.component)
            .to_string();
        let name = Path::new(&effective)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        (name, effective)
    } else {
        let comp = args.comp.load()?;
        homeboy::component::validate_local_path(&comp)?;
        let expanded = shellexpand::tilde(&comp.local_path).to_string();
        (comp.id.clone(), expanded)
    };

    let workflow = run_main_audit_workflow(AuditRunWorkflowArgs {
        component_id: resolved_id,
        source_path: resolved_path,
        conventions: args.conventions,
        fix: args.fix,
        write: args.write,
        max_iterations: args.max_iterations,
        scoring: AuditConvergenceScoring {
            warning_weight: args.warning_weight,
            info_weight: args.info_weight,
        },
        verification: AuditVerificationToggles {
            lint_smoke: !args.no_lint_smoke,
            test_smoke: !args.no_test_smoke,
        },
        only_kinds,
        exclude_kinds,
        only_labels: args.only,
        exclude_labels: args.exclude,
        ratchet: args.ratchet,
        baseline: args.baseline_args.baseline,
        ignore_baseline: args.baseline_args.ignore_baseline,
        changed_since: args.changed_since,
        json_summary: args.json_summary,
        preview: args.preview,
    })?;

    Ok(report::from_main_workflow(workflow))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::BaselineArgs;
    use homeboy::code_audit::run::default_audit_exit_code;
    use homeboy::code_audit::{AuditFinding, AuditSummary, CodeAuditResult, Finding, Severity};
    use homeboy::refactor::{
        auto::{ApplyChunkResult, ChunkStatus, FixSafetyTier, InsertionKind},
        build_chunk_verifier, finding_fingerprint, score_delta, weighted_finding_score_with,
        AuditConvergenceScoring,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-audit-command-{name}-{nanos}"))
    }

    fn mk_result(outliers_found: usize, findings_len: usize) -> CodeAuditResult {
        CodeAuditResult {
            component_id: "component".to_string(),
            source_path: "/tmp/component".to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found,
                alignment_score: Some(1.0),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: (0..findings_len)
                .map(|i| Finding {
                    file: format!("src/file{i}.rs"),
                    convention: "Example".to_string(),
                    severity: Severity::Warning,
                    description: "desc".to_string(),
                    suggestion: "suggest".to_string(),
                    kind: AuditFinding::MissingMethod,
                })
                .collect(),
            duplicate_groups: vec![],
        }
    }

    #[test]
    fn test_default_audit_exit_code_full_uses_outliers() {
        let result = mk_result(2, 0);
        assert_eq!(default_audit_exit_code(&result, false), 1);
    }

    #[test]
    fn test_default_audit_exit_code_scoped_uses_findings() {
        let result = mk_result(71, 0);
        assert_eq!(default_audit_exit_code(&result, true), 0);

        let result = mk_result(0, 1);
        assert_eq!(default_audit_exit_code(&result, true), 1);
    }

    #[test]
    fn test_run_fix_write_applies_preflight_checked_method_stub() {
        use homeboy::refactor::auto::{
            self, Fix, FixPolicy, FixResult, Insertion, PreflightContext,
        };

        let root = tmp_dir("fix-write-applies-preflight-checked-method-stub");
        fs::create_dir_all(root.join("commands")).unwrap();
        fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

        let mut fix_result = FixResult {
            fixes: vec![Fix {
                file: "commands/bad.rs".to_string(),
                required_methods: vec!["run".to_string(), "helper".to_string()],
                required_registrations: vec![],
                insertions: vec![Insertion {
                    kind: InsertionKind::MethodStub,
                    finding: AuditFinding::MissingMethod,
                    safety_tier: InsertionKind::MethodStub.safety_tier(),
                    auto_apply: false,
                    blocked_reason: None,
                    preflight: None,
                    code: "\npub fn helper() {\n    todo!(\"helper\")\n}\n".to_string(),
                    description: "Add missing method helper()".to_string(),
                }],
                applied: false,
            }],
            new_files: vec![],
            decompose_plans: vec![],
            skipped: vec![],
            chunk_results: vec![],
            total_insertions: 1,
            files_modified: 0,
        };

        let summary = auto::apply_fix_policy(
            &mut fix_result,
            true,
            &FixPolicy::default(),
            &PreflightContext { root: &root },
        );

        assert_eq!(summary.auto_apply_insertions, 0);
        assert_eq!(summary.preflight_failures, 0);

        let insertion = &fix_result.fixes[0].insertions[0];
        assert_eq!(insertion.finding, AuditFinding::MissingMethod);
        assert!(matches!(insertion.kind, InsertionKind::MethodStub));
        assert_eq!(insertion.safety_tier, FixSafetyTier::PlanOnly);
        assert!(!insertion.auto_apply);
        assert!(insertion.preflight.is_some());

        let mut auto_subset = auto::auto_apply_subset(&fix_result);
        let modified = auto::apply_fixes(&mut auto_subset.fixes, &root);
        assert_eq!(modified, 0);

        let content = fs::read_to_string(root.join("commands/bad.rs")).unwrap();
        assert_eq!(content, "pub fn run() {}\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_run_fix_only_import_add_filters_method_stub() {
        use homeboy::refactor::auto::{
            self, Fix, FixPolicy, FixResult, Insertion, PreflightContext,
        };

        let root = tmp_dir("fix-only-import-add");
        fs::create_dir_all(root.join("commands")).unwrap();
        fs::write(
            root.join("commands/bad.rs"),
            "pub fn run() -> CmdResult<()> {\n    Ok(())\n}\n",
        )
        .unwrap();

        let mut fix_result = FixResult {
            fixes: vec![Fix {
                file: "commands/bad.rs".to_string(),
                required_methods: vec!["run".to_string()],
                required_registrations: vec![],
                insertions: vec![
                    Insertion {
                        kind: InsertionKind::ImportAdd,
                        finding: AuditFinding::MissingImport,
                        safety_tier: InsertionKind::ImportAdd.safety_tier(),
                        auto_apply: false,
                        blocked_reason: None,
                        preflight: None,
                        code: "use super::CmdResult;\n".to_string(),
                        description: "Add missing import CmdResult".to_string(),
                    },
                    Insertion {
                        kind: InsertionKind::MethodStub,
                        finding: AuditFinding::MissingMethod,
                        safety_tier: InsertionKind::MethodStub.safety_tier(),
                        auto_apply: false,
                        blocked_reason: None,
                        preflight: None,
                        code: "\npub fn helper() {\n    todo!(\"helper\")\n}\n".to_string(),
                        description: "Add missing method helper()".to_string(),
                    },
                ],
                applied: false,
            }],
            new_files: vec![],
            decompose_plans: vec![],
            skipped: vec![],
            chunk_results: vec![],
            total_insertions: 2,
            files_modified: 0,
        };

        let policy = FixPolicy {
            only: Some(vec![AuditFinding::MissingImport]),
            exclude: vec![],
        };

        auto::apply_fix_policy(
            &mut fix_result,
            false,
            &policy,
            &PreflightContext { root: &root },
        );

        assert_eq!(fix_result.fixes.len(), 1);
        assert_eq!(fix_result.fixes[0].insertions.len(), 1);

        let insertion = &fix_result.fixes[0].insertions[0];
        assert!(insertion.auto_apply);
        assert_eq!(insertion.finding, AuditFinding::MissingImport);
        assert!(matches!(insertion.kind, InsertionKind::ImportAdd));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_chunk_verifier_accepts_clean_changed_files() {
        let root = tmp_dir("chunk-verifier-clean");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();

        let result = {
            let baseline = homeboy::code_audit::audit_path_scoped(
                "audit-fix-verify",
                &root.to_string_lossy(),
                &["commands/good_one.rs".to_string()],
                None,
            )
            .unwrap();
            let verifier = build_chunk_verifier(&root, &baseline.findings, vec![]);
            verifier(&ApplyChunkResult {
                chunk_id: "fix:1".to_string(),
                files: vec!["commands/good_one.rs".to_string()],
                status: ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: None,
                error: None,
            })
        };

        assert_eq!(result.unwrap(), "scoped_reaudit_no_new_findings");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_chunk_verifier_rejects_new_findings_in_changed_files() {
        if homeboy::extension::find_extension_for_file_ext("rs", "fingerprint").is_none() {
            eprintln!("SKIP: no Rust fingerprint extension installed");
            return;
        }

        let root = tmp_dir("chunk-verifier-dirty");
        fs::create_dir_all(root.join("src")).unwrap();

        fs::write(root.join("src/target.rs"), "// empty\n").unwrap();

        let baseline = homeboy::code_audit::audit_path_scoped(
            "audit-fix-verify",
            &root.to_string_lossy(),
            &["src/target.rs".to_string()],
            None,
        )
        .unwrap();
        assert!(
            baseline.findings.is_empty(),
            "Baseline for empty file should have no findings"
        );

        fs::write(root.join("src/target.rs"), "pub fn placeholder() {}\n").unwrap();

        let result = {
            let verifier = build_chunk_verifier(&root, &baseline.findings, vec![]);
            verifier(&ApplyChunkResult {
                chunk_id: "fix:1".to_string(),
                files: vec!["src/target.rs".to_string()],
                status: ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: None,
                error: None,
            })
        };

        assert!(
            result.is_ok(),
            "Verifier should pass when re-audit finds no new findings: {:?}",
            result
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_finding_fingerprint_comparison() {
        let baseline_finding = Finding {
            convention: "naming".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Existing finding".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::NamingMismatch,
        };

        let same_finding = Finding {
            convention: "naming".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Existing finding".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::NamingMismatch,
        };

        let new_finding = Finding {
            convention: "duplication".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Duplicate function `foo`".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::DuplicateFunction,
        };

        let baseline_fp = finding_fingerprint(&baseline_finding);
        let same_fp = finding_fingerprint(&same_finding);
        let new_fp = finding_fingerprint(&new_finding);

        assert_eq!(
            baseline_fp, same_fp,
            "Identical findings should have same fingerprint"
        );
        assert_ne!(
            baseline_fp, new_fp,
            "Different findings should have different fingerprints"
        );

        let baseline_set: std::collections::HashSet<String> =
            vec![baseline_fp].into_iter().collect();

        let post_findings = [&same_finding, &new_finding];
        let new_findings: Vec<_> = post_findings
            .iter()
            .filter(|f| !baseline_set.contains(&finding_fingerprint(f)))
            .collect();

        assert_eq!(
            new_findings.len(),
            1,
            "Should detect exactly one new finding"
        );
        assert_eq!(new_findings[0].kind, AuditFinding::DuplicateFunction);
    }

    #[test]
    fn test_chunk_verifier_appends_smoke_result() {
        let root = tmp_dir("chunk-verifier-smoke-success");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();

        let baseline = homeboy::code_audit::audit_path_scoped(
            "audit-fix-verify",
            &root.to_string_lossy(),
            &["commands/good_one.rs".to_string()],
            None,
        )
        .unwrap();

        let smoke = |_chunk: &ApplyChunkResult| Ok("lint_smoke_passed".to_string());

        let result = {
            let verifier = build_chunk_verifier(&root, &baseline.findings, vec![&smoke]);
            verifier(&ApplyChunkResult {
                chunk_id: "fix:1".to_string(),
                files: vec!["commands/good_one.rs".to_string()],
                status: ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: None,
                error: None,
            })
        };

        assert_eq!(
            result.unwrap(),
            "scoped_reaudit_no_new_findings+lint_smoke_passed"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_chunk_verifier_rejects_smoke_failure() {
        let root = tmp_dir("chunk-verifier-smoke-failure");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();

        let baseline = homeboy::code_audit::audit_path_scoped(
            "audit-fix-verify",
            &root.to_string_lossy(),
            &["commands/good_one.rs".to_string()],
            None,
        )
        .unwrap();

        let smoke = |_chunk: &ApplyChunkResult| Err("lint smoke failed".to_string());

        let result = {
            let verifier = build_chunk_verifier(&root, &baseline.findings, vec![&smoke]);
            verifier(&ApplyChunkResult {
                chunk_id: "fix:1".to_string(),
                files: vec!["commands/good_one.rs".to_string()],
                status: ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: None,
                error: None,
            })
        };

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "lint smoke failed");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_run_fix_write_stops_when_no_safe_changes_apply() {
        let root = tmp_dir("fix-write-no-safe-changes");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

        let args = AuditArgs {
            comp: PositionalComponentArgs {
                component: root.to_string_lossy().to_string(),
                path: None,
            },
            conventions: false,
            fix: true,
            write: true,
            ratchet: false,
            max_iterations: 3,
            warning_weight: 3,
            info_weight: 1,
            no_lint_smoke: false,
            no_test_smoke: false,
            only: vec!["duplicate_function".to_string()],
            exclude: vec![],
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: true,
            },
            changed_since: None,
            json_summary: false,
            preview: false,
        };

        let (output, _code) =
            run(args, &crate::commands::GlobalArgs {}).expect("audit fix should run");

        match output {
            AuditCommandOutput::Fix { iterations, .. } => {
                assert!(!iterations.is_empty(), "expected at least one iteration");
                let any_applied = iterations.iter().any(|i| i.applied_chunks > 0);
                assert!(
                    any_applied,
                    "expected at least one iteration to apply changes, got: {:?}",
                    iterations.iter().map(|i| &i.status).collect::<Vec<_>>()
                );
            }
            other => panic!(
                "expected AuditCommandOutput::Fix, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_weighted_finding_score_prefers_warning_reduction() {
        let result = CodeAuditResult {
            component_id: "demo".to_string(),
            source_path: "/tmp/demo".to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found: 2,
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![
                Finding {
                    convention: "Test".to_string(),
                    severity: Severity::Warning,
                    file: "src/a.rs".to_string(),
                    description: "Warning finding".to_string(),
                    suggestion: "Fix it".to_string(),
                    kind: AuditFinding::MissingMethod,
                },
                Finding {
                    convention: "Test".to_string(),
                    severity: Severity::Info,
                    file: "src/b.rs".to_string(),
                    description: "Info finding".to_string(),
                    suggestion: "Investigate".to_string(),
                    kind: AuditFinding::MissingImport,
                },
            ],
            duplicate_groups: vec![],
        };

        assert_eq!(
            weighted_finding_score_with(&result, AuditConvergenceScoring::default()),
            4
        );
        assert_eq!(
            weighted_finding_score_with(
                &result,
                AuditConvergenceScoring {
                    warning_weight: 5,
                    info_weight: 2,
                }
            ),
            7
        );
    }

    #[test]
    fn test_iteration_score_delta_zero_means_no_progress() {
        let before = CodeAuditResult {
            component_id: "demo".to_string(),
            source_path: "/tmp/demo".to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found: 1,
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![Finding {
                convention: "Test".to_string(),
                severity: Severity::Warning,
                file: "src/a.rs".to_string(),
                description: "Warning finding".to_string(),
                suggestion: "Fix it".to_string(),
                kind: AuditFinding::MissingMethod,
            }],
            duplicate_groups: vec![],
        };
        let after = before.clone();

        let score_delta = score_delta(&before, &after, AuditConvergenceScoring::default());

        assert_eq!(score_delta, 0);
    }

    #[test]
    fn test_score_delta_uses_configured_weights() {
        let before = CodeAuditResult {
            component_id: "demo".to_string(),
            source_path: "/tmp/demo".to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found: 2,
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![
                Finding {
                    convention: "Test".to_string(),
                    severity: Severity::Warning,
                    file: "src/a.rs".to_string(),
                    description: "Warning finding".to_string(),
                    suggestion: "Fix it".to_string(),
                    kind: AuditFinding::MissingMethod,
                },
                Finding {
                    convention: "Test".to_string(),
                    severity: Severity::Info,
                    file: "src/b.rs".to_string(),
                    description: "Info finding".to_string(),
                    suggestion: "Investigate".to_string(),
                    kind: AuditFinding::MissingImport,
                },
            ],
            duplicate_groups: vec![],
        };

        let after = CodeAuditResult {
            findings: vec![Finding {
                convention: "Test".to_string(),
                severity: Severity::Info,
                file: "src/b.rs".to_string(),
                description: "Info finding".to_string(),
                suggestion: "Investigate".to_string(),
                kind: AuditFinding::MissingImport,
            }],
            ..before.clone()
        };

        assert_eq!(
            score_delta(
                &before,
                &after,
                AuditConvergenceScoring {
                    warning_weight: 5,
                    info_weight: 1,
                }
            ),
            5
        );
    }
}
