//! helpers — extracted from contracts.rs.

use crate::code_audit::conventions::AuditFinding;
use std::path::Path;


pub(crate) fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}
