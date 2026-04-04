use crate::code_audit::AuditFinding;
use crate::core::refactor::auto::{Insertion, InsertionKind, NewFile, RefactorPrimitive};

pub fn insertion(
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

pub fn line_replacement(
    finding: AuditFinding,
    line: usize,
    old_text: String,
    new_text: String,
    description: String,
) -> Insertion {
    insertion(
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

pub fn tagged_line_replacement(
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

pub fn range_removal(
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

pub fn tagged_range_removal(
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

pub fn import_add(finding: AuditFinding, code: String, description: String) -> Insertion {
    insertion(InsertionKind::ImportAdd, finding, code, description)
}

pub fn tagged_import_add(
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

pub fn visibility_change(
    finding: AuditFinding,
    line: usize,
    from: String,
    to: String,
    description: String,
) -> Insertion {
    insertion(
        InsertionKind::VisibilityChange { line, from, to },
        finding,
        String::new(),
        description,
    )
}

pub fn tagged_visibility_change(
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

pub fn doc_line_removal(finding: AuditFinding, line: usize, description: String) -> Insertion {
    insertion(
        InsertionKind::DocLineRemoval { line },
        finding,
        String::new(),
        description,
    )
}

pub fn manual_only(mut insertion: Insertion) -> Insertion {
    insertion.manual_only = true;
    insertion
}

/// Mark an insertion as manual-only with a specific blocked reason.
pub fn manual_blocked(mut insertion: Insertion, reason: String) -> Insertion {
    insertion.manual_only = true;
    insertion.blocked_reason = Some(reason);
    insertion
}

pub fn new_file(
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
