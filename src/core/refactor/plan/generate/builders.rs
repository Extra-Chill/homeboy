use crate::code_audit::AuditFinding;
use crate::core::refactor::auto::{FixSafetyTier, Insertion, InsertionKind, NewFile};

pub(crate) fn insertion(
    kind: InsertionKind,
    finding: AuditFinding,
    code: String,
    description: String,
) -> Insertion {
    Insertion {
        safety_tier: kind.safety_tier(),
        kind,
        finding,
        auto_apply: false,
        blocked_reason: None,
        preflight: None,
        code,
        description,
    }
}

pub(crate) fn new_file(
    finding: AuditFinding,
    safety_tier: FixSafetyTier,
    file: String,
    content: String,
    description: String,
) -> NewFile {
    NewFile {
        file,
        finding,
        safety_tier,
        auto_apply: false,
        blocked_reason: None,
        preflight: None,
        content,
        description,
        written: false,
    }
}
