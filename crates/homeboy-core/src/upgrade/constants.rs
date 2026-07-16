pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) const CRATES_IO_API: &str = "https://crates.io/api/v1/crates/homeboy";

pub(crate) const GITHUB_RELEASES_API: &str =
    "https://api.github.com/repos/Extra-Chill/homeboy/releases/latest";

/// Number of attempts to read back the active binary version after a
/// successful upgrade swap. The first read can race the just-replaced binary
/// (atomic rename not yet observable on PATH, stale resolution, etc.), so we
/// retry before declaring the upgrade unverifiable.
pub(crate) const VERIFY_READBACK_ATTEMPTS: u32 = 5;

/// Delay between version read-back attempts after a successful upgrade swap.
pub(crate) const VERIFY_READBACK_DELAY: std::time::Duration = std::time::Duration::from_millis(200);
