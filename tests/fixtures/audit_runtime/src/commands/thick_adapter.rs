// Fixture command-layer file: trips the thin_command_adapter policy because it
// contains the configured ORCHESTRATION_MARKER inside a command path.
pub fn run_command() {
    // ORCHESTRATION_MARKER: this command module carries orchestration weight.
    let _ = orchestrate();
}

fn orchestrate() -> u32 {
    42
}
