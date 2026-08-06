mod bulk;
mod guard;
mod io;
mod sections;
mod settings;

pub use bulk::{show, ShowOutput};
pub use guard::{
    detect_manual_changelog_edit, generated_file_mutation_is_authorized,
    generated_file_mutation_is_authorized_for, ChangelogGuardViolation,
};
pub use io::{
    discover_changelog_relative_path, read_component_snapshots, resolve_changelog_path,
    ChangelogSnapshotData, FinalizedReleaseSnapshot, CHANGELOG_CANDIDATES,
    INITIAL_CHANGELOG_CONTENT,
};
pub use sections::{count_unreleased_entries, get_latest_finalized_version};
// Reached only by this crate's own `version` module (the bump/finalize path).
// Kept at `pub(crate)` so `changelog::…` call sites still resolve while the
// functions stay subject to rustc's dead-code analysis.
pub(crate) use sections::{finalize_next_section, finalize_with_generated_entries};
pub use settings::{resolve_effective_settings, EffectiveChangelogSettings};
