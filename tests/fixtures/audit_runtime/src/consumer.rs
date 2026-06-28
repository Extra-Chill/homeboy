// Fixture file: cross-references `exports::referenced_helper` so the symbol
// graph resolves a real cross-file edge (suppressing an unreferenced-export
// finding for `referenced_helper`). `wire_up` is itself exported and called by
// nobody, so it surfaces as its own unreferenced_export finding — a second
// stable symbol-graph assertion. Both are captured in EXPECTED_FINDINGS.
use crate::exports::referenced_helper;

pub fn wire_up() -> u32 {
    referenced_helper() + 1
}
