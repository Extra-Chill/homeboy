use crate::code_audit::{fixer, CodeAuditResult};
use std::path::Path;

pub fn generate_audit_fixes(result: &CodeAuditResult, root: &Path) -> fixer::FixResult {
    fixer::generate_fixes_impl(result, root)
}
