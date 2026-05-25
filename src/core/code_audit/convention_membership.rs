use super::conventions::{AuditFinding, Deviation};
use super::fingerprint::FileFingerprint;
use crate::core::component::AuditConfig;

const GENERIC_UTILITY_SUFFIXES: &[&str] = &[
    "Base",
    "Handler",
    "Handlers",
    "Helper",
    "Helpers",
    "Projector",
    "Projectors",
];

pub(super) fn member_requirement_deviation(
    kind: AuditFinding,
    description_label: &str,
    suggestion_verb: &str,
    expected: &str,
    expected_suffix: &str,
    group_name: &str,
) -> Deviation {
    Deviation {
        kind,
        description: format!("{}: {}", description_label, expected),
        suggestion: format!(
            "{} {}{} to match the convention in {}",
            suggestion_verb, expected, expected_suffix, group_name
        ),
    }
}

pub(super) fn declared_trait_name(fp: &FileFingerprint) -> Option<String> {
    let re = regex::Regex::new(r"(?m)^\s*trait\s+([A-Za-z_][A-Za-z0-9_]*)\b").ok()?;
    re.captures(&fp.content)
        .and_then(|cap| cap.get(1))
        .map(|m| m.as_str().to_string())
}

pub(super) fn declares_type_subject(fp: &FileFingerprint) -> bool {
    fp.type_name.is_some() || !fp.type_names.is_empty()
}

pub(super) fn is_utility_like_file(fp: &FileFingerprint, audit_config: &AuditConfig) -> bool {
    let names_to_check: Vec<&str> = if !fp.type_names.is_empty() {
        fp.type_names.iter().map(|s| s.as_str()).collect()
    } else {
        fp.type_name.as_deref().into_iter().collect()
    };

    declared_trait_name(fp).is_some()
        || names_to_check.iter().any(|name| {
            GENERIC_UTILITY_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix))
                || audit_config
                    .utility_suffixes
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
        })
}

pub(super) fn is_convention_exception(fp: &FileFingerprint, audit_config: &AuditConfig) -> bool {
    let normalized = fp.relative_path.replace('\\', "/");
    audit_config
        .convention_exception_globs
        .iter()
        .any(|pattern| glob_match::glob_match(pattern, &normalized))
}
