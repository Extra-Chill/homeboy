use crate::auto::{Insertion, InsertionKind, NewFile, RefactorPrimitive};
use homeboy_audit_contract::AuditFinding;

pub(crate) fn insertion(
    kind: InsertionKind,
    finding: AuditFinding,
    code: String,
    description: String,
) -> Insertion {
    Insertion {
        primitive: None,
        kind,
        finding,
        manual_only: false,
        auto_apply: false,
        blocked_reason: None,
        code,
        description,
    }
}

pub(crate) fn tagged_insertion(
    primitive: RefactorPrimitive,
    kind: InsertionKind,
    finding: AuditFinding,
    code: String,
    description: String,
) -> Insertion {
    Insertion {
        primitive: Some(primitive),
        kind,
        finding,
        manual_only: false,
        auto_apply: false,
        blocked_reason: None,
        code,
        description,
    }
}

pub(crate) fn tagged_line_replacement(
    primitive: RefactorPrimitive,
    finding: AuditFinding,
    line: usize,
    old_text: String,
    new_text: String,
    description: String,
) -> Insertion {
    tagged_insertion(
        primitive,
        InsertionKind::LineReplacement {
            line,
            old_text,
            new_text,
        },
        finding,
        String::new(),
        description,
    )
}

pub(crate) fn range_removal(
    finding: AuditFinding,
    start_line: usize,
    end_line: usize,
    description: String,
) -> Insertion {
    insertion(
        InsertionKind::FunctionRemoval {
            start_line,
            end_line,
        },
        finding,
        String::new(),
        description,
    )
}

pub(crate) fn tagged_range_removal(
    primitive: RefactorPrimitive,
    finding: AuditFinding,
    start_line: usize,
    end_line: usize,
    description: String,
) -> Insertion {
    tagged_insertion(
        primitive,
        InsertionKind::FunctionRemoval {
            start_line,
            end_line,
        },
        finding,
        String::new(),
        description,
    )
}

pub(crate) fn tagged_import_add(
    primitive: RefactorPrimitive,
    finding: AuditFinding,
    code: String,
    description: String,
) -> Insertion {
    tagged_insertion(
        primitive,
        InsertionKind::ImportAdd,
        finding,
        code,
        description,
    )
}

pub(crate) fn tagged_visibility_change(
    primitive: RefactorPrimitive,
    finding: AuditFinding,
    line: usize,
    from: String,
    to: String,
    description: String,
) -> Insertion {
    tagged_insertion(
        primitive,
        InsertionKind::VisibilityChange { line, from, to },
        finding,
        String::new(),
        description,
    )
}

pub(crate) fn function_removal(
    finding: AuditFinding,
    start_line: usize,
    end_line: usize,
    code: String,
    description: String,
) -> Insertion {
    insertion(
        InsertionKind::FunctionRemoval {
            start_line,
            end_line,
        },
        finding,
        code,
        description,
    )
}

pub(crate) fn tagged_doc_reference_update(
    primitive: RefactorPrimitive,
    finding: AuditFinding,
    line: usize,
    old_ref: String,
    new_ref: String,
    code: String,
    description: String,
) -> Insertion {
    tagged_insertion(
        primitive,
        InsertionKind::DocReferenceUpdate {
            line,
            old_ref,
            new_ref,
        },
        finding,
        code,
        description,
    )
}

pub(crate) fn doc_line_removal(
    finding: AuditFinding,
    line: usize,
    description: String,
) -> Insertion {
    insertion(
        InsertionKind::DocLineRemoval { line },
        finding,
        String::new(),
        description,
    )
}

pub(crate) fn tagged_doc_line_removal(
    primitive: RefactorPrimitive,
    finding: AuditFinding,
    line: usize,
    description: String,
) -> Insertion {
    tagged_insertion(
        primitive,
        InsertionKind::DocLineRemoval { line },
        finding,
        String::new(),
        description,
    )
}

pub(crate) fn manual_only(mut insertion: Insertion) -> Insertion {
    insertion.manual_only = true;
    insertion
}

/// Mark an insertion as manual-only with a specific blocked reason.
pub(crate) fn manual_blocked(mut insertion: Insertion, reason: String) -> Insertion {
    insertion.manual_only = true;
    insertion.blocked_reason = Some(reason);
    insertion
}

pub(crate) fn new_file(
    finding: AuditFinding,
    file: String,
    content: String,
    description: String,
) -> NewFile {
    NewFile {
        file,
        primitive: None,
        finding,
        manual_only: false,
        auto_apply: false,
        blocked_reason: None,
        content,
        description,
        written: false,
    }
}
