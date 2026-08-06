pub(crate) const VERSION: &str = homeboy_product_identity::product_version();

pub(crate) const GITHUB_RELEASES_API: &str =
    "https://api.github.com/repos/Extra-Chill/homeboy/releases/latest";

/// Release list endpoint. `releases/latest` answers "what is the newest tag",
/// which is not the question an installer needs answered — it cannot say
/// whether that release ships an asset for the running target, nor name an
/// older release that does (#11750).
pub(crate) const GITHUB_RELEASES_LIST_API: &str =
    "https://api.github.com/repos/Extra-Chill/homeboy/releases?per_page=30";

/// Base for release asset downloads. Kept beside the API constant so the URL
/// surfaced in a 404 failure is built from the same source as the installer's.
pub(crate) const RELEASE_ASSET_DOWNLOAD_BASE: &str =
    "https://github.com/Extra-Chill/homeboy/releases";

/// Environment variable read by the binary installer to pin the release tag it
/// downloads from. Absent, the installer keeps using `latest/download`.
pub(crate) const RELEASE_TAG_ENV: &str = "HOMEBOY_UPGRADE_RELEASE_TAG";

/// Timeout for the release list request. Matches the single-release fetch: the
/// update path must never hang a command.
pub(crate) const RELEASE_CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Number of attempts to read back the active binary version after a
/// successful upgrade swap. The first read can race the just-replaced binary
/// (atomic rename not yet observable on PATH, stale resolution, etc.), so we
/// retry before declaring the upgrade unverifiable.
pub(crate) const VERIFY_READBACK_ATTEMPTS: u32 = 5;

/// Delay between version read-back attempts after a successful upgrade swap.
pub(crate) const VERIFY_READBACK_DELAY: std::time::Duration = std::time::Duration::from_millis(200);
