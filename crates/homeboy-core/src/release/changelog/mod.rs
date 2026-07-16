mod bulk;
mod guard;
mod io;
mod sections;
mod settings;

pub use bulk::{show, ShowOutput};
pub use guard::{detect_changelog_edit, ChangelogGuardViolation};
pub use io::{
    discover_changelog_relative_path, read_component_snapshots, resolve_changelog_path,
    ChangelogSnapshotData, FinalizedReleaseSnapshot, CHANGELOG_CANDIDATES,
    INITIAL_CHANGELOG_CONTENT,
};
pub use sections::{
    count_unreleased_entries, extract_last_release_snapshot, finalize_next_section,
    finalize_with_generated_entries, get_latest_finalized_version, get_unreleased_entries,
};
pub use settings::{resolve_effective_settings, EffectiveChangelogSettings};
