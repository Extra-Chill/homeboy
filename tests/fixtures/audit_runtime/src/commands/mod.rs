// Fixture module facade. REGRESSION GUARD for #10558.
//
// `mod.rs` is an INDEX FILE: `walker::INDEX_FILES` excludes it from convention
// discovery because module roots organize other files rather than being peers,
// and including them produces false "missing method" findings. That reasoning is
// about convention SIBLING detection and has no bearing on a whole-file term
// scan — but source policies used to borrow the convention corpus, so every
// `mod.rs`, `lib.rs`, and `main.rs` in a repository was unscannable by ANY
// source policy regardless of configuration (181 files on homeboy itself).
//
// This file carries the fixture's configured forbidden term below. If the
// source-policy corpus ever regresses to the convention corpus, the
// `source_policy_violation::src/commands/mod.rs` entry disappears from
// EXPECTED_FINDINGS and the snapshot harness fails.
pub fn facade_entry() -> &'static str {
    "forbiddenmarker"
}
