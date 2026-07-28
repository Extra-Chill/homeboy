// Fixture command-layer file: trips the thin_command_adapter policy because it
// contains the configured ORCHESTRATION_MARKER inside a command path.
//
// SECOND ROLE — REGRESSION GUARD for #10558's singleton-group filter. This is
// the only non-index file in `src/commands/`, so convention discovery drops it:
// `groups_from_dir_files` discards any group with fewer than two members
// because a convention needs peers to exist. Correct for conventions, and
// meaningless for a term scan — yet source policies used to inherit it (82 more
// unscannable files on homeboy itself, on top of the 181 index files).
//
// The forbidden term below must therefore produce
// `source_policy_violation::src/commands/thick_adapter.rs`. If the
// source-policy corpus regresses to the convention corpus, that entry vanishes
// and the snapshot harness fails.
pub fn run_command() {
    // ORCHESTRATION_MARKER: this command module carries orchestration weight.
    let _ = orchestrate();
    let _ = solo_directory_marker();
}

fn orchestrate() -> u32 {
    42
}

fn solo_directory_marker() -> &'static str {
    "forbiddenmarker"
}
