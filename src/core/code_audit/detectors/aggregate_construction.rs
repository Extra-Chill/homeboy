//! Direct aggregate construction detector.
//!
//! Core consumes language-neutral fingerprint facts emitted by extensions. It
//! does not parse language syntax here; each language extension owns
//! recognizing its own aggregate literal and construction seam syntax.

use std::collections::{BTreeMap, BTreeSet};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

const MIN_OCCURRENCES: usize = 3;
const MIN_FILES: usize = 2;
const MIN_SHARED_FIELDS: usize = 2;

pub(in crate::core::code_audit) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    detect_direct_aggregate_construction(fingerprints)
}

fn detect_direct_aggregate_construction(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut seams_by_type: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut literals_by_type: BTreeMap<
        String,
        Vec<(&str, &crate::core::extension::AggregateLiteral)>,
    > = BTreeMap::new();

    for fp in fingerprints {
        if super::walker::is_test_path(&fp.relative_path) {
            continue;
        }

        for seam in &fp.aggregate_construction_seams {
            seams_by_type
                .entry(seam.type_name.clone())
                .or_default()
                .insert(seam.method.clone());
        }

        for literal in &fp.aggregate_literals {
            literals_by_type
                .entry(literal.type_name.clone())
                .or_default()
                .push((&fp.relative_path, literal));
        }
    }

    let mut findings = Vec::new();

    for (type_name, literals) in literals_by_type {
        let Some(seams) = seams_by_type.get(&type_name) else {
            continue;
        };

        let file_count = literals
            .iter()
            .map(|(file, _)| *file)
            .collect::<BTreeSet<_>>()
            .len();

        if literals.len() < MIN_OCCURRENCES || file_count < MIN_FILES {
            continue;
        }

        let mut field_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut top_file_counts: BTreeMap<String, usize> = BTreeMap::new();
        for (file, literal) in &literals {
            *top_file_counts.entry((*file).to_string()).or_insert(0) += 1;
            for field in &literal.fields {
                *field_counts.entry(field.clone()).or_insert(0) += 1;
            }
        }

        let shared_fields = field_counts
            .iter()
            .filter_map(|(field, count)| {
                if *count >= MIN_OCCURRENCES {
                    Some(field.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if shared_fields.len() < MIN_SHARED_FIELDS {
            continue;
        }

        let mut top_files = top_file_counts.into_iter().collect::<Vec<_>>();
        top_files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        top_files.truncate(3);

        let anchor = top_files
            .first()
            .map(|(file, _)| file.clone())
            .unwrap_or_else(|| "<unknown>".to_string());
        let seam_display = seams
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let top_files_display = top_files
            .iter()
            .map(|(file, count)| format!("{} ({})", file, count))
            .collect::<Vec<_>>()
            .join(", ");

        findings.push(Finding {
            convention: "direct_aggregate_construction".to_string(),
            severity: Severity::Warning,
            file: anchor,
            description: format!(
                "Direct aggregate construction: `{}` is built inline {} time(s) across {} file(s) despite canonical construction seam(s) [{}]. Repeated fields: [{}]. Top files: {}.",
                type_name,
                literals.len(),
                file_count,
                seam_display,
                shared_fields.join(", "),
                top_files_display
            ),
            suggestion: format!(
                "Route repeated `{}` construction through the existing builder/factory/helper seam instead of repeating aggregate literals.",
                type_name
            ),
            kind: AuditFinding::DirectAggregateConstruction,
        });
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extension::{AggregateConstructionSeam, AggregateLiteral};

    fn file(
        path: &str,
        literals: Vec<AggregateLiteral>,
        seams: Vec<AggregateConstructionSeam>,
    ) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            aggregate_literals: literals,
            aggregate_construction_seams: seams,
            ..Default::default()
        }
    }

    fn literal(type_name: &str, fields: &[&str]) -> AggregateLiteral {
        AggregateLiteral {
            type_name: type_name.to_string(),
            fields: fields.iter().map(|field| field.to_string()).collect(),
            line: 1,
        }
    }

    fn seam(type_name: &str, method: &str) -> AggregateConstructionSeam {
        AggregateConstructionSeam {
            type_name: type_name.to_string(),
            method: method.to_string(),
            line: 1,
        }
    }

    #[test]
    fn test_run() {
        let files = [
            file(
                "src/report.rs",
                vec![literal("SummaryReport", &["status", "count"])],
                vec![seam("SummaryReport", "new")],
            ),
            file(
                "src/a.rs",
                vec![literal("SummaryReport", &["status", "count"])],
                vec![],
            ),
            file(
                "src/b.rs",
                vec![literal("SummaryReport", &["status", "count"])],
                vec![],
            ),
        ];
        let refs = files.iter().collect::<Vec<_>>();

        assert_eq!(run(&refs).len(), 1);
    }

    #[test]
    fn flags_repeated_direct_aggregate_when_canonical_seam_exists() {
        let files = [
            file(
                "src/order.rs",
                vec![literal("DispatchPlan", &["status", "steps", "dry_run"])],
                vec![seam("DispatchPlan", "builder")],
            ),
            file(
                "src/a.rs",
                vec![literal("DispatchPlan", &["status", "steps", "dry_run"])],
                vec![],
            ),
            file(
                "src/b.rs",
                vec![literal("DispatchPlan", &["status", "steps", "dry_run"])],
                vec![],
            ),
        ];
        let refs = files.iter().collect::<Vec<_>>();

        let findings = run(&refs);

        assert_eq!(findings.len(), 1, "expected one finding, got {findings:?}");
        assert_eq!(findings[0].kind, AuditFinding::DirectAggregateConstruction);
        assert!(findings[0].description.contains("DispatchPlan"));
        assert!(findings[0].description.contains("builder"));
    }

    #[test]
    fn does_not_flag_repeated_aggregate_without_construction_seam() {
        let files = [
            file("src/a.rs", vec![literal("Point", &["x", "y"])], vec![]),
            file("src/b.rs", vec![literal("Point", &["x", "y"])], vec![]),
            file("src/c.rs", vec![literal("Point", &["x", "y"])], vec![]),
        ];
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs).is_empty());
    }

    #[test]
    fn ignores_test_paths() {
        let files = [
            file("src/plan.rs", vec![], vec![seam("BuildReport", "new")]),
            file(
                "tests/a.rs",
                vec![literal("BuildReport", &["status", "count"])],
                vec![],
            ),
            file(
                "tests/b.rs",
                vec![literal("BuildReport", &["status", "count"])],
                vec![],
            ),
            file(
                "tests/c.rs",
                vec![literal("BuildReport", &["status", "count"])],
                vec![],
            ),
        ];
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs).is_empty());
    }
}
