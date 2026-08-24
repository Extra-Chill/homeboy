//! Twin type declaration detector.
//!
//! Two aggregates declared with the same field shape are one type written
//! twice. Keeping them in step is then a manual obligation, and the compiler
//! only helps where a conversion between them is *total* — every declared field
//! copied. Where it is not, adding a field to one side compiles clean and
//! silently fails to reach the other.
//!
//! That is not hypothetical for this repo. `Component`/`RawComponent` were 44
//! identical fields joined by two hand-written conversions; a field added to
//! one conversion and not the other shipped a config value that round-tripped
//! to disk and was invisible to the subsystem that read it (#10220, fixed in
//! `0ac903bcc`). `NewTraceRunRecord`/`TraceRunRecord` were a verbatim copy of a
//! seven-field record where the table assigns no column, so the distinction the
//! copy existed to express did not exist.
//!
//! Core stays language-neutral: this consumes the `aggregate_definitions` and
//! `aggregate_projections` facts that language extensions already emit, and
//! never parses source.
//!
//! # Why a shape must be fully resolved
//!
//! Grouping on field *names* alone reports parse boundaries as twins. A type
//! that takes `Option<Value>` off the wire and a type that narrows it to
//! `Option<Map<String, Value>>` share every field name and are not duplicates —
//! the narrowing is the work. Each field's resolved `type_id` separates "one
//! type written twice" from "two representations of one concept", without core
//! knowing what those type names mean.
//!
//! That only holds when the types are actually resolved. Measured against this
//! repo, 48% of declared fields carry no `type_id` — extensions resolve plain
//! and qualified names but not generic applications, so every `Option<..>` field
//! is unresolved. Treating two unresolved fields as equal is not conservative,
//! it is name-only matching wearing a type check: it is exactly what let the
//! `Option<Value>` / `Option<Map<String, Value>>` pair above group as twins.
//!
//! So a definition with any unresolved field type is not evidence of anything
//! and is skipped. That trades recall for precision — 4 reported clusters here
//! against 27 under name-only fallback — which is the right default for a
//! detector that gates. Recall improves as extensions resolve more types; no
//! change is needed here when they do.

use std::collections::{BTreeMap, BTreeSet};

use homeboy_audit_contract::{AggregateDefinitionFact, AggregateProjectionFact};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

/// Below this, identical shapes are routinely coincidence rather than copying:
/// pairs like `{path, line}` or `{x, y, width, height}` recur across unrelated
/// concepts. Every twin this repo has actually had to fix carried at least
/// seven fields.
const MIN_FIELDS: usize = 5;

/// One declared field, as it contributes to a type's shape.
type FieldShape = (String, String);

pub(crate) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut by_shape: BTreeMap<Vec<FieldShape>, BTreeMap<String, (String, u32)>> = BTreeMap::new();
    let mut projections: Vec<&AggregateProjectionFact> = Vec::new();

    for fp in fingerprints {
        if super::walker::is_test_path(&fp.relative_path) {
            continue;
        }
        for definition in &fp.aggregate_definitions {
            if definition.fields.len() < MIN_FIELDS || !is_fully_resolved(definition) {
                continue;
            }
            by_shape
                .entry(shape_of(definition))
                .or_default()
                .entry(definition.type_id.clone())
                .or_insert_with(|| {
                    (
                        fp.relative_path.clone(),
                        u32::try_from(definition.location.line).unwrap_or(0),
                    )
                });
        }
        projections.extend(fp.aggregate_projections.iter());
    }

    let mut findings = Vec::new();

    for (shape, twins) in by_shape {
        if twins.len() < 2 {
            continue;
        }

        let type_ids = twins.keys().cloned().collect::<Vec<_>>();
        let covered = totally_projected_pairs(&type_ids, shape.len(), &projections);
        let pair_count = type_ids.len() * (type_ids.len() - 1);
        let fully_protected = covered == pair_count;

        let (anchor_file, anchor_line) = twins
            .values()
            .next()
            .cloned()
            .unwrap_or_else(|| ("<unknown>".to_string(), 0));

        let declared = twins
            .iter()
            .map(|(type_id, (file, line))| format!("`{}` ({}:{})", type_id, file, line))
            .collect::<Vec<_>>()
            .join(", ");
        let field_names = shape
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let (severity, protection, suggestion) = if fully_protected {
            (
                Severity::Info,
                "Every conversion between them copies all declared fields, so the compiler currently rejects a field added to one side and not the other."
                    .to_string(),
                format!(
                    "The copy is compiler-enforced today and is not a correctness hazard. Collapse it only if the {} fields have no reason to be declared twice — a layering boundary or a differing serialized shape can be reason enough.",
                    shape.len()
                ),
            )
        } else {
            (
                Severity::Warning,
                "No conversion between them copies every declared field, so a field added to one side can compile clean and never reach the other."
                    .to_string(),
                "Collapse them onto one declaration, or make the conversion total so the compiler enforces the copy."
                    .to_string(),
            )
        };

        findings.push(Finding {
            convention: "twin_type_declaration".to_string(),
            severity,
            file: anchor_file,
            description: format!(
                "Twin type declarations: {} declare the same {} fields with the same types [{}]. {}",
                declared,
                shape.len(),
                field_names,
                protection
            ),
            suggestion,
            kind: AuditFinding::TwinTypeDeclaration,
            line: Some(anchor_line),
        });
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

/// Whether every declared field carries a resolved type.
///
/// One unresolved field makes the whole shape unverifiable: the rest of the
/// signature can match perfectly while the unresolved field differs, which is
/// the parse-boundary false positive this detector must not report.
fn is_fully_resolved(definition: &AggregateDefinitionFact) -> bool {
    definition
        .fields
        .iter()
        .all(|field| field.type_id.as_ref().is_some_and(|id| !id.is_empty()))
}

/// A type's shape: its declared fields as `(name, type)` pairs, ordered so two
/// declarations that differ only in field order still compare equal.
fn shape_of(definition: &AggregateDefinitionFact) -> Vec<FieldShape> {
    let mut shape = definition
        .fields
        .iter()
        .map(|field| {
            (
                field.name.clone(),
                field.type_id.clone().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    shape.sort();
    shape
}

/// Count ordered type pairs joined by at least one conversion that copies every
/// declared field.
///
/// A total conversion is the compiler's enforcement point: written as an
/// exhaustive literal, adding a field to the target is a hard error at that
/// site. A conversion that copies only some fields leaves the rest to a default
/// or an update-from-other, which is exactly where a new field goes missing.
fn totally_projected_pairs(
    type_ids: &[String],
    field_count: usize,
    projections: &[&AggregateProjectionFact],
) -> usize {
    let members = type_ids.iter().collect::<BTreeSet<_>>();
    let mut total_pairs: BTreeSet<(&str, &str)> = BTreeSet::new();

    for projection in projections {
        if !members.contains(&projection.source_type_id)
            || !members.contains(&projection.target_type_id)
            || projection.source_type_id == projection.target_type_id
        {
            continue;
        }
        let copied = projection
            .field_mappings
            .iter()
            .map(|mapping| mapping.target_field.as_str())
            .collect::<BTreeSet<_>>();
        if copied.len() >= field_count {
            total_pairs.insert((
                projection.source_type_id.as_str(),
                projection.target_type_id.as_str(),
            ));
        }
    }

    total_pairs.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_audit_contract::{AggregateFieldFact, FactLocation, ProjectionFieldFact};

    fn file(
        path: &str,
        definitions: Vec<AggregateDefinitionFact>,
        projections: Vec<AggregateProjectionFact>,
    ) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            aggregate_definitions: definitions,
            aggregate_projections: projections,
            ..Default::default()
        }
    }

    fn definition(type_id: &str, fields: &[(&str, &str)]) -> AggregateDefinitionFact {
        AggregateDefinitionFact {
            type_id: type_id.to_string(),
            fields: fields
                .iter()
                .map(|(name, type_id)| AggregateFieldFact {
                    name: (*name).to_string(),
                    type_id: Some((*type_id).to_string()),
                })
                .collect(),
            location: FactLocation {
                line: 10,
                ..Default::default()
            },
        }
    }

    fn projection(source: &str, target: &str, fields: &[&str]) -> AggregateProjectionFact {
        AggregateProjectionFact {
            source_type_id: source.to_string(),
            target_type_id: target.to_string(),
            callable_id: format!("{}::from", target),
            field_mappings: fields
                .iter()
                .map(|field| ProjectionFieldFact {
                    source_field: (*field).to_string(),
                    target_field: (*field).to_string(),
                })
                .collect(),
            location: FactLocation::default(),
        }
    }

    const SEVEN: &[(&str, &str)] = &[
        ("run_id", "String"),
        ("component_id", "String"),
        ("rig_id", "Option<String>"),
        ("scenario_id", "String"),
        ("status", "String"),
        ("baseline_status", "Option<String>"),
        ("metadata_json", "Value"),
    ];

    /// The `NewTraceRunRecord`/`TraceRunRecord` shape: one record declared twice,
    /// with nothing converting between them.
    #[test]
    fn an_unconverted_identical_shape_is_a_warning() {
        let files = [file(
            "src/records.rs",
            vec![
                definition("core::records::NewTraceRunRecord", SEVEN),
                definition("core::records::TraceRunRecord", SEVEN),
            ],
            vec![],
        )];

        let findings = run(&files.iter().collect::<Vec<_>>());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].kind, AuditFinding::TwinTypeDeclaration);
        assert!(findings[0].description.contains("NewTraceRunRecord"));
        assert!(findings[0].description.contains("TraceRunRecord"));
        assert!(
            findings[0]
                .description
                .contains("can compile clean and never reach the other"),
            "an unprotected twin must say what breaks: {}",
            findings[0].description
        );
    }

    /// The `PreparedDaemonExec`/`PreparedDaemonExecRequest` shape: still one type
    /// written twice, but every conversion copies every field, so the compiler
    /// already rejects a half-landed change. Real duplication, not a hazard.
    #[test]
    fn a_totally_converted_identical_shape_is_downgraded_to_info() {
        let fields = SEVEN.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        let files = [file(
            "src/driver.rs",
            vec![
                definition("core::driver::PreparedExec", SEVEN),
                definition("core::driver::PreparedExecRequest", SEVEN),
            ],
            vec![
                projection(
                    "core::driver::PreparedExecRequest",
                    "core::driver::PreparedExec",
                    &fields,
                ),
                projection(
                    "core::driver::PreparedExec",
                    "core::driver::PreparedExecRequest",
                    &fields,
                ),
            ],
        )];

        let findings = run(&files.iter().collect::<Vec<_>>());

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].severity,
            Severity::Info,
            "a compiler-enforced copy is not a correctness hazard"
        );
        assert!(findings[0]
            .description
            .contains("compiler currently rejects"));
    }

    /// A conversion that copies only some fields is not enforcement: the
    /// remaining fields come from a default or an update-from-other, which is
    /// exactly where a newly added field goes missing.
    #[test]
    fn a_partial_conversion_does_not_count_as_protection() {
        let files = [file(
            "src/driver.rs",
            vec![
                definition("core::driver::Alpha", SEVEN),
                definition("core::driver::Beta", SEVEN),
            ],
            vec![projection(
                "core::driver::Alpha",
                "core::driver::Beta",
                &["run_id", "component_id"],
            )],
        )];

        let findings = run(&files.iter().collect::<Vec<_>>());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    /// The `DomBoxElement`/`BrowserElement` shape: every field name matches, but
    /// one type narrows what the other accepts off the wire. The narrowing is
    /// the work, not duplication.
    #[test]
    fn identical_field_names_with_differing_types_are_not_twins() {
        let wire = SEVEN
            .iter()
            .map(|(name, _)| (*name, "Value"))
            .collect::<Vec<_>>();
        let files = [file(
            "src/dom.rs",
            vec![
                definition("core::dom::DomBoxElement", SEVEN),
                definition("core::dom::BrowserElement", &wire),
            ],
            vec![],
        )];

        assert!(run(&files.iter().collect::<Vec<_>>()).is_empty());
    }

    /// Small shapes recur across unrelated concepts. `{path, line}` is not a
    /// duplicated type.
    #[test]
    fn shapes_below_the_field_floor_are_not_reported() {
        let small: &[(&str, &str)] = &[("path", "String"), ("line", "usize")];
        let files = [file(
            "src/small.rs",
            vec![
                definition("core::a::Location", small),
                definition("core::b::Anchor", small),
            ],
            vec![],
        )];

        assert!(run(&files.iter().collect::<Vec<_>>()).is_empty());
    }

    /// Field declaration order is not part of a type's shape.
    #[test]
    fn field_order_does_not_hide_a_twin() {
        let mut reordered = SEVEN.to_vec();
        reordered.reverse();
        let files = [file(
            "src/records.rs",
            vec![
                definition("core::records::Forward", SEVEN),
                definition("core::records::Reversed", &reordered),
            ],
            vec![],
        )];

        assert_eq!(run(&files.iter().collect::<Vec<_>>()).len(), 1);
    }

    /// An unresolved field type makes the whole shape unverifiable. Two
    /// declarations can agree on every resolved field and differ precisely on
    /// the one nobody could resolve.
    #[test]
    fn a_shape_with_any_unresolved_field_type_is_not_evidence() {
        let mut partial = definition("core::a::Alpha", SEVEN);
        partial.fields[0].type_id = None;
        let mut twin = definition("core::b::Beta", SEVEN);
        twin.fields[0].type_id = None;

        let files = [file("src/a.rs", vec![partial, twin], vec![])];

        assert!(
            run(&files.iter().collect::<Vec<_>>()).is_empty(),
            "unresolved types must not be treated as matching each other"
        );
    }

    /// Test scaffolding routinely mirrors production shapes on purpose.
    #[test]
    fn test_paths_are_not_scanned() {
        let files = [file(
            "tests/fixtures.rs",
            vec![
                definition("tests::fixtures::Alpha", SEVEN),
                definition("tests::fixtures::Beta", SEVEN),
            ],
            vec![],
        )];

        assert!(run(&files.iter().collect::<Vec<_>>()).is_empty());
    }
}
