use crate::code_audit::conventions::{AuditFinding, Language};
use crate::core::refactor::plan::generate::extract_signatures_from_items;

use crate::refactor::auto::apply::apply_insertions_to_content;
use crate::refactor::auto::policy::blocked_reason_from_preflight;
use crate::refactor::auto::{
    Fix, FixSafetyTier, Insertion, InsertionKind, NewFile, PreflightCheck, PreflightContext,
    PreflightReport, PreflightStatus,
};

pub fn run_insertion_preflight(
    file: &str,
    insertion: &Insertion,
    context: &PreflightContext<'_>,
) -> Option<PreflightReport> {
    match insertion.finding {
        AuditFinding::MissingMethod
        | AuditFinding::MissingRegistration
        | AuditFinding::MissingInterface
        | AuditFinding::NamespaceMismatch => {
            let abs_path = context.root.join(file);
            let content = std::fs::read_to_string(&abs_path).ok()?;
            let language = Language::from_path(&abs_path);
            let simulated =
                apply_insertions_to_content(&content, std::slice::from_ref(insertion), &language);

            let checks = vec![
                collision_check(&content, insertion),
                syntax_shape_check(&simulated, insertion, &language),
            ];
            Some(finalize_report(checks))
        }
        AuditFinding::UnreferencedExport => {
            let abs_path = context.root.join(file);
            let content = std::fs::read_to_string(&abs_path).ok()?;
            let language = Language::from_path(&abs_path);
            let simulated =
                apply_insertions_to_content(&content, std::slice::from_ref(insertion), &language);

            let changed = simulated != content;
            let checks = vec![PreflightCheck {
                name: "visibility_changed".to_string(),
                passed: changed,
                detail: if changed {
                    "visibility qualifier was narrowed successfully".to_string()
                } else {
                    "visibility qualifier was not found or already narrowed".to_string()
                },
            }];
            Some(finalize_report(checks))
        }
        AuditFinding::DuplicateFunction => {
            let abs_path = context.root.join(file);
            let content = std::fs::read_to_string(&abs_path).ok()?;
            let language = Language::from_path(&abs_path);

            let mut checks = Vec::new();

            if matches!(insertion.kind, InsertionKind::TraitUse) {
                let has_class = content.contains("class ");
                checks.push(PreflightCheck {
                    name: "class_exists".to_string(),
                    passed: has_class,
                    detail: if has_class {
                        "target file contains a class definition".to_string()
                    } else {
                        "target file does not contain a class definition".to_string()
                    },
                });

                let trait_code = insertion.code.trim();
                let already_present = content.lines().any(|line| line.trim() == trait_code);
                checks.push(PreflightCheck {
                    name: "trait_use_absent".to_string(),
                    passed: !already_present,
                    detail: if already_present {
                        format!("trait use `{}` already exists in file", trait_code)
                    } else {
                        format!("trait use `{}` is not yet present", trait_code)
                    },
                });

                let simulated = apply_insertions_to_content(
                    &content,
                    std::slice::from_ref(insertion),
                    &language,
                );
                checks.push(syntax_shape_check(&simulated, insertion, &language));

                return Some(finalize_report(checks));
            }

            if let InsertionKind::FunctionRemoval {
                start_line,
                end_line,
            } = &insertion.kind
            {
                let line_count = content.lines().count();
                let range_valid = *start_line >= 1 && *end_line <= line_count;
                checks.push(PreflightCheck {
                    name: "function_boundaries".to_string(),
                    passed: range_valid,
                    detail: if range_valid {
                        format!(
                            "Function found at lines {}–{} (file has {} lines)",
                            start_line, end_line, line_count
                        )
                    } else {
                        format!(
                            "Line range {}–{} is out of bounds (file has {} lines)",
                            start_line, end_line, line_count
                        )
                    },
                });

                if range_valid {
                    let simulated = apply_insertions_to_content(
                        &content,
                        std::slice::from_ref(insertion),
                        &language,
                    );
                    let still_valid = simulated != content;
                    checks.push(PreflightCheck {
                        name: "removal_applied".to_string(),
                        passed: still_valid,
                        detail: if still_valid {
                            "Function removal modifies the file as expected".to_string()
                        } else {
                            "Removal produced no change — function may have already been removed"
                                .to_string()
                        },
                    });
                }
            }

            if checks.is_empty() {
                None
            } else {
                Some(finalize_report(checks))
            }
        }
        AuditFinding::OrphanedTest => {
            let abs_path = context.root.join(file);
            let content = std::fs::read_to_string(&abs_path).ok()?;
            let language = Language::from_path(&abs_path);

            if let InsertionKind::FunctionRemoval {
                start_line,
                end_line,
            } = &insertion.kind
            {
                let line_count = content.lines().count();
                let range_valid = *start_line >= 1 && *end_line <= line_count;
                let mut checks = vec![PreflightCheck {
                    name: "function_boundaries".to_string(),
                    passed: range_valid,
                    detail: if range_valid {
                        format!(
                            "Orphaned test found at lines {}–{} (file has {} lines)",
                            start_line, end_line, line_count
                        )
                    } else {
                        format!(
                            "Line range {}–{} is out of bounds (file has {} lines)",
                            start_line, end_line, line_count
                        )
                    },
                }];

                if range_valid {
                    let simulated = apply_insertions_to_content(
                        &content,
                        std::slice::from_ref(insertion),
                        &language,
                    );
                    let still_valid = simulated != content;
                    checks.push(PreflightCheck {
                        name: "removal_applied".to_string(),
                        passed: still_valid,
                        detail: if still_valid {
                            "Orphaned test removal modifies the file as expected".to_string()
                        } else {
                            "Removal produced no change — test may have already been removed"
                                .to_string()
                        },
                    });
                }

                Some(finalize_report(checks))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn run_fix_preflight(fix: &mut Fix, context: &PreflightContext<'_>, write: bool) {
    if fix.insertions.is_empty() {
        return;
    }

    let abs_path = context.root.join(&fix.file);
    let Ok(content) = std::fs::read_to_string(&abs_path) else {
        return;
    };
    let language = Language::from_path(&abs_path);
    let simulated = apply_insertions_to_content(&content, &fix.insertions, &language);

    let mut extra_checks = Vec::new();
    if !fix.required_methods.is_empty() {
        extra_checks.push(required_methods_check(
            &simulated,
            &language,
            &fix.required_methods,
        ));
    }
    if !fix.required_registrations.is_empty() {
        extra_checks.push(required_registrations_check(
            &simulated,
            &fix.required_registrations,
        ));
    }

    for insertion in &mut fix.insertions {
        if insertion.safety_tier != FixSafetyTier::SafeWithChecks {
            continue;
        }

        if let Some(report) = &mut insertion.preflight {
            report.checks.extend(extra_checks.clone());
            *report = finalize_report(report.checks.clone());
        }

        insertion.auto_apply = if !write {
            true
        } else {
            insertion.preflight.as_ref().is_some_and(|report| {
                matches!(
                    report.status,
                    PreflightStatus::Passed | PreflightStatus::NotApplicable
                )
            })
        };

        insertion.blocked_reason = if insertion.auto_apply {
            None
        } else {
            Some(
                insertion
                    .preflight
                    .as_ref()
                    .and_then(blocked_reason_from_preflight)
                    .unwrap_or_else(|| {
                        "Blocked: requires preflight validation before auto-write".to_string()
                    }),
            )
        };
    }
}

pub fn run_new_file_preflight(
    new_file: &NewFile,
    context: &PreflightContext<'_>,
) -> Option<PreflightReport> {
    match new_file.finding {
        AuditFinding::DuplicateFunction => {
            let abs = context.root.join(&new_file.file);
            let parent_exists = abs
                .parent()
                .map(|p| p.exists() || p == context.root)
                .unwrap_or(false);

            Some(finalize_report(vec![
                PreflightCheck {
                    name: "file_absent".to_string(),
                    passed: !abs.exists(),
                    detail: if abs.exists() {
                        format!("{} already exists — will not overwrite", new_file.file)
                    } else {
                        format!("{} does not already exist", new_file.file)
                    },
                },
                PreflightCheck {
                    name: "content_nonempty".to_string(),
                    passed: !new_file.content.trim().is_empty(),
                    detail: if new_file.content.trim().is_empty() {
                        "generated trait content is empty".to_string()
                    } else {
                        "generated trait content is non-empty".to_string()
                    },
                },
                PreflightCheck {
                    name: "parent_exists".to_string(),
                    passed: parent_exists,
                    detail: if parent_exists {
                        "parent directory exists or is project root".to_string()
                    } else {
                        format!(
                            "parent directory {} does not exist",
                            abs.parent()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default()
                        )
                    },
                },
            ]))
        }
        _ => None,
    }
}

fn finalize_report(checks: Vec<PreflightCheck>) -> PreflightReport {
    let status = if checks.iter().all(|check| check.passed) {
        PreflightStatus::Passed
    } else {
        PreflightStatus::Failed
    };

    PreflightReport { status, checks }
}

fn collision_check(content: &str, insertion: &Insertion) -> PreflightCheck {
    let collision_free = !content.contains(&insertion.code);
    PreflightCheck {
        name: "collision".to_string(),
        passed: collision_free,
        detail: if collision_free {
            "target file does not already contain this generated code".to_string()
        } else {
            "target file already contains identical generated code".to_string()
        },
    }
}

fn syntax_shape_check(content: &str, insertion: &Insertion, language: &Language) -> PreflightCheck {
    let detail_prefix = match insertion.finding {
        AuditFinding::MissingMethod => "generated method stub",
        AuditFinding::MissingRegistration => "generated registration/constructor",
        AuditFinding::MissingInterface => "generated type conformance",
        AuditFinding::NamespaceMismatch => "generated namespace declaration",
        _ => "generated content",
    };

    let parsed_ok = match language {
        Language::Php => {
            !extract_signatures_from_items(content, language).is_empty()
                || content.contains("class ")
        }
        Language::Rust => {
            !extract_signatures_from_items(content, language).is_empty() || content.contains("fn ")
        }
        Language::JavaScript | Language::TypeScript => {
            !extract_signatures_from_items(content, language).is_empty()
                || content.contains("function ")
        }
        Language::Unknown => true,
    };

    PreflightCheck {
        name: "syntax_shape".to_string(),
        passed: parsed_ok,
        detail: if parsed_ok {
            format!(
                "{} preserves parseable structural signatures",
                detail_prefix
            )
        } else {
            format!(
                "{} produced content that no longer matches expected signature shapes",
                detail_prefix
            )
        },
    }
}

fn required_methods_check(
    content: &str,
    language: &Language,
    required_methods: &[String],
) -> PreflightCheck {
    let missing: Vec<String> = required_methods
        .iter()
        .filter(|method| !method_present(content, language, method))
        .cloned()
        .collect();

    PreflightCheck {
        name: "required_methods".to_string(),
        passed: missing.is_empty(),
        detail: if missing.is_empty() {
            format!(
                "required methods preserved: {}",
                required_methods.join(", ")
            )
        } else {
            format!(
                "missing required methods after simulation: {}",
                missing.join(", ")
            )
        },
    }
}

fn method_present(content: &str, language: &Language, method: &str) -> bool {
    let escaped = regex::escape(method);
    let pattern = match language {
        Language::Php => format!(r"\bfunction\s+{}\b", escaped),
        Language::Rust => format!(r"\bfn\s+{}\b", escaped),
        Language::JavaScript | Language::TypeScript => {
            format!(r"\b(function\s+{}\b|{}\s*\()", escaped, escaped)
        }
        Language::Unknown => return content.contains(method),
    };

    regex::Regex::new(&pattern)
        .map(|re| re.is_match(content))
        .unwrap_or_else(|_| content.contains(method))
}

fn required_registrations_check(
    content: &str,
    required_registrations: &[String],
) -> PreflightCheck {
    let missing: Vec<String> = required_registrations
        .iter()
        .filter(|registration| !content.contains(registration.as_str()))
        .cloned()
        .collect();

    PreflightCheck {
        name: "required_registrations".to_string(),
        passed: missing.is_empty(),
        detail: if missing.is_empty() {
            format!(
                "required registrations preserved: {}",
                required_registrations.join(", ")
            )
        } else {
            format!(
                "missing required registrations after simulation: {}",
                missing.join(", ")
            )
        },
    }
}
