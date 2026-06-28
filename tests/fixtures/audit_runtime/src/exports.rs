// Fixture file: exercises the cross-file symbol graph / reference resolution.
//
// `referenced_helper` is exported AND called from `consumer.rs`, so the symbol
// graph must resolve that cross-file edge and SUPPRESS an unreferenced-export
// finding for it. `orphaned_helper` is exported but referenced by nobody, so it
// MUST surface as an unreferenced_export finding. This pins symbol-resolution
// behavior so a config/grammar regression that breaks the graph is caught.
pub fn referenced_helper() -> u32 {
    7
}

pub fn orphaned_helper() -> u32 {
    11
}
