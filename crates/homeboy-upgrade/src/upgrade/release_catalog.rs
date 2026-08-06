//! Which published releases this host can actually install.
//!
//! `homeboy upgrade` used to resolve exactly one release — the newest tag — and
//! hand it to a shell installer that downloads
//! `releases/latest/download/homeboy-<target>.tar.xz`. When a release is
//! published without the running platform's asset (#11749), that installer 404s
//! and the operator has no way forward: `--force` retries the same missing
//! asset and building from source is not viable on a host that routes builds to
//! CI. `--check` made it worse by reporting `update_available: true` for a
//! release this platform cannot install.
//!
//! This module answers the narrower, honest question: *which is the newest
//! release that ships an artifact for the running target*. Selection is a pure
//! function over a release list so the fallback, the pin, and the `--check`
//! verdict are all testable without network access.
//!
//! Target detection is deliberately allowed to fail. An unrecognized
//! OS/architecture pair yields `None`, and every caller reports "the running
//! target could not be determined" rather than guessing a triple and producing
//! a confidently wrong 404.

use homeboy_core::error::{Error, Result};
use semver::Version;
use serde::Deserialize;

use super::constants::{
    GITHUB_RELEASES_LIST_API, RELEASE_ASSET_DOWNLOAD_BASE, RELEASE_CATALOG_TIMEOUT, VERSION,
};

/// Release artifacts are named `homeboy-<target triple>.tar.xz`, and the
/// installer downloads the checksum sidecar alongside the archive. A release
/// missing either one cannot be installed, so both are required before a
/// release counts as installable for a target.
const ASSET_ARCHIVE_SUFFIX: &str = ".tar.xz";
const ASSET_CHECKSUM_SUFFIX: &str = ".tar.xz.sha256";

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseEntry {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

impl ReleaseEntry {
    /// Release tags are `v<semver>`; the rest of the upgrade surface speaks
    /// bare semver, so both spellings are kept rather than reconstructed.
    pub fn version(&self) -> &str {
        self.tag_name
            .strip_prefix('v')
            .unwrap_or(self.tag_name.as_str())
    }

    pub fn has_asset_for(&self, target: &str) -> bool {
        let archive = asset_archive_name(target);
        let checksum = asset_checksum_name(target);
        let has = |wanted: &str| self.assets.iter().any(|asset| asset.name == wanted);
        has(&archive) && has(&checksum)
    }

    fn to_release_ref(&self) -> ReleaseRef {
        ReleaseRef {
            tag: self.tag_name.clone(),
            version: self.version().to_string(),
        }
    }
}

/// One published release, in both spellings the upgrade surface needs: the tag
/// the installer downloads from, and the semver the version gate compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseRef {
    pub tag: String,
    pub version: String,
}

/// The release a binary upgrade has committed to installing, together with the
/// target triple its asset was resolved for.
///
/// The triple is carried rather than re-derived at the failure site so the
/// error names the target that was *actually* looked for, including the honest
/// `None` when the running platform could not be identified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedRelease {
    pub tag: String,
    pub version: String,
    pub target: Option<String>,
}

impl SelectedRelease {
    pub fn new(release: ReleaseRef, target: Option<&str>) -> Self {
        Self {
            tag: release.tag,
            version: release.version,
            target: target.map(str::to_string),
        }
    }

    /// The URL the installer resolves for this release, or `None` when no
    /// target triple is known and therefore no asset name can be built.
    pub fn asset_url(&self) -> Option<String> {
        self.target
            .as_deref()
            .map(|target| asset_url(Some(&self.tag), target))
    }
}

/// The outcome of asking "what can this host install?".
///
/// `newest` and `installable` are the same release on a healthy release train.
/// They diverge exactly when the newest release is missing this target's
/// asset, and that divergence is the thing an operator has to be told about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallableSelection {
    /// Newest published stable release, whatever its asset inventory.
    pub newest: Option<ReleaseRef>,
    /// Newest published stable release carrying an asset for the target.
    pub installable: Option<ReleaseRef>,
    /// Releases newer than `installable` that were passed over because they
    /// ship no asset for the target, newest first.
    pub skipped: Vec<ReleaseRef>,
}

impl InstallableSelection {
    /// Plain-language explanation of a fallback, phrased for the upgrade that
    /// is about to happen. `None` when the newest release is installable —
    /// there is nothing to explain.
    pub fn upgrade_fallback_notice(&self, target: &str) -> Option<String> {
        let installable = self.installable.as_ref()?;
        let newest = self.newest.as_ref()?;
        (newest.version != installable.version).then(|| {
            format!(
                "{} has no {} asset; upgrading to {} instead.",
                newest.tag, target, installable.tag
            )
        })
    }

    /// The same divergence phrased for `--check`, which is reporting rather
    /// than upgrading and must not claim an upgrade is under way.
    pub fn check_fallback_notice(&self, target: &str) -> Option<String> {
        let installable = self.installable.as_ref()?;
        let newest = self.newest.as_ref()?;
        (newest.version != installable.version).then(|| {
            format!(
                "{} has no {} asset; {} is the newest installable release.",
                newest.tag, target, installable.tag
            )
        })
    }

    pub fn skipped_versions(&self) -> Vec<String> {
        self.skipped
            .iter()
            .map(|entry| entry.version.clone())
            .collect()
    }
}

pub fn asset_archive_name(target: &str) -> String {
    format!("homeboy-{target}{ASSET_ARCHIVE_SUFFIX}")
}

fn asset_checksum_name(target: &str) -> String {
    format!("homeboy-{target}{ASSET_CHECKSUM_SUFFIX}")
}

/// The exact URL the installer resolves for `tag`, or the floating
/// `latest/download` URL when no tag was pinned. Surfaced in the 404 error so
/// diagnosing a missing asset does not require listing release assets by hand.
pub fn asset_url(tag: Option<&str>, target: &str) -> String {
    let archive = asset_archive_name(target);
    match tag {
        Some(tag) => format!("{RELEASE_ASSET_DOWNLOAD_BASE}/download/{tag}/{archive}"),
        None => format!("{RELEASE_ASSET_DOWNLOAD_BASE}/latest/download/{archive}"),
    }
}

/// The Rust target triple this binary is running as, or `None` when the
/// OS/architecture pair is not one Homeboy publishes assets for.
///
/// This intentionally mirrors the case statement in the binary installer
/// rather than inventing a broader mapping: the set of triples that can be
/// resolved here is exactly the set the installer can download. Anything else
/// is reported as undetermined, because a guessed triple would produce a 404
/// that blames the release instead of the platform.
pub fn running_target_triple() -> Option<&'static str> {
    target_triple_for(std::env::consts::OS, std::env::consts::ARCH)
}

pub fn target_triple_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") | ("linux", "arm64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") | ("macos", "arm64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// Human-readable description of the running platform, used when the triple
/// cannot be determined so the message names what was actually observed.
pub fn running_platform_description() -> String {
    format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Newest-first stable releases with a comparable semver tag.
///
/// Drafts and prereleases are excluded: they are not upgrade targets. Tags that
/// do not parse as semver are excluded too — an incomparable tag cannot be
/// honestly ordered against the running version, and silently ranking it would
/// make selection depend on GitHub's response order.
fn ordered_stable_releases(releases: &[ReleaseEntry]) -> Vec<(Version, &ReleaseEntry)> {
    let mut ordered: Vec<(Version, &ReleaseEntry)> = releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            Version::parse(release.version())
                .ok()
                .map(|version| (version, release))
        })
        .collect();
    ordered.sort_by(|left, right| right.0.cmp(&left.0));
    ordered
}

/// Select the newest release, and the newest release this target can install.
///
/// When `target` is `None` the running platform is undetermined, so asset
/// availability is not verified at all: `installable` is the newest release and
/// nothing is reported as skipped. Pretending to have verified an asset for an
/// unknown triple would be the same dishonesty the 404 came from.
pub fn select_installable(releases: &[ReleaseEntry], target: Option<&str>) -> InstallableSelection {
    let ordered = ordered_stable_releases(releases);
    let newest = ordered.first().map(|(_, release)| release.to_release_ref());

    let Some(target) = target else {
        return InstallableSelection {
            installable: newest.clone(),
            newest,
            skipped: Vec::new(),
        };
    };

    let mut skipped = Vec::new();
    let mut installable = None;
    for (_, release) in &ordered {
        if release.has_asset_for(target) {
            installable = Some(release.to_release_ref());
            break;
        }
        skipped.push(release.to_release_ref());
    }

    if installable.is_none() {
        // Nothing is installable, so nothing was "passed over on the way to"
        // a usable release. Reporting the whole catalog as skipped would bury
        // the real finding: this target has no artifact anywhere.
        skipped.clear();
    }

    InstallableSelection {
        newest,
        installable,
        skipped,
    }
}

/// Locate one specific release by tag, accepting either `v0.332.0` or
/// `0.332.0` so an operator can paste whichever spelling they have in hand.
pub fn find_release(releases: &[ReleaseEntry], requested: &str) -> Option<ReleaseRef> {
    let wanted = requested.trim().trim_start_matches('v');
    releases
        .iter()
        .filter(|release| !release.draft)
        .find(|release| release.version() == wanted)
        .map(ReleaseEntry::to_release_ref)
}

/// Whether a specific release carries this target's asset. `None` target means
/// the platform is undetermined, so installability is unknown rather than true.
pub fn release_installs_on(
    releases: &[ReleaseEntry],
    requested: &str,
    target: Option<&str>,
) -> Option<bool> {
    let wanted = requested.trim().trim_start_matches('v');
    let target = target?;
    releases
        .iter()
        .find(|release| release.version() == wanted)
        .map(|release| release.has_asset_for(target))
}

/// Installable tags, newest first, for error hints that have to name a way out.
pub fn installable_tags(
    releases: &[ReleaseEntry],
    target: Option<&str>,
    limit: usize,
) -> Vec<String> {
    let Some(target) = target else {
        return Vec::new();
    };
    ordered_stable_releases(releases)
        .into_iter()
        .filter(|(_, release)| release.has_asset_for(target))
        .map(|(_, release)| release.tag_name.clone())
        .take(limit)
        .collect()
}

pub(crate) fn fetch_release_catalog() -> Result<Vec<ReleaseEntry>> {
    fetch_release_catalog_at(GITHUB_RELEASES_LIST_API)
}

pub(crate) fn fetch_release_catalog_at(url: &str) -> Result<Vec<ReleaseEntry>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("homeboy/{}", VERSION))
        .timeout(RELEASE_CATALOG_TIMEOUT)
        .build()
        .map_err(|e| Error::internal_io(e.to_string(), Some("create HTTP client".to_string())))?;

    client
        .get(url)
        .send()
        .map_err(|e| Error::internal_io(e.to_string(), Some("list GitHub releases".to_string())))?
        .json::<Vec<ReleaseEntry>>()
        .map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some("parse GitHub release list response".to_string()),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, targets: &[&str]) -> ReleaseEntry {
        let mut assets = Vec::new();
        for target in targets {
            assets.push(ReleaseAsset {
                name: asset_archive_name(target),
            });
            assets.push(ReleaseAsset {
                name: asset_checksum_name(target),
            });
        }
        // Every real release also carries platform-independent artifacts; they
        // must never be mistaken for a target asset.
        assets.push(ReleaseAsset {
            name: "dist-manifest.json".to_string(),
        });
        assets.push(ReleaseAsset {
            name: "source.tar.gz".to_string(),
        });

        ReleaseEntry {
            tag_name: tag.to_string(),
            draft: false,
            prerelease: false,
            assets,
        }
    }

    const LINUX: &str = "x86_64-unknown-linux-gnu";
    const MAC: &str = "aarch64-apple-darwin";

    /// The reported case: the newest release has no Linux asset, but the one
    /// before it does and is still far ahead of the running controller.
    #[test]
    fn falls_back_to_the_newest_release_with_a_target_asset() {
        let releases = vec![
            release("v0.333.0", &[MAC]),
            release("v0.332.0", &[LINUX, MAC]),
            release("v0.331.0", &[LINUX, MAC]),
        ];

        let selection = select_installable(&releases, Some(LINUX));

        assert_eq!(
            selection.newest.as_ref().map(|entry| entry.tag.as_str()),
            Some("v0.333.0")
        );
        assert_eq!(
            selection
                .installable
                .as_ref()
                .map(|entry| entry.tag.as_str()),
            Some("v0.332.0")
        );
        assert_eq!(selection.skipped_versions(), vec!["0.333.0".to_string()]);
    }

    /// The fallback is only useful if it is stated. This is the exact sentence
    /// the issue asked for.
    #[test]
    fn fallback_notice_names_both_releases_and_the_target() {
        let releases = vec![release("v0.333.0", &[MAC]), release("v0.332.0", &[LINUX])];

        let selection = select_installable(&releases, Some(LINUX));

        assert_eq!(
            selection.upgrade_fallback_notice(LINUX).as_deref(),
            Some("v0.333.0 has no x86_64-unknown-linux-gnu asset; upgrading to v0.332.0 instead.")
        );
        assert_eq!(
            selection.check_fallback_notice(LINUX).as_deref(),
            Some(
                "v0.333.0 has no x86_64-unknown-linux-gnu asset; v0.332.0 is the newest installable release."
            )
        );
    }

    #[test]
    fn no_notice_when_the_newest_release_is_installable() {
        let releases = vec![release("v0.333.0", &[LINUX]), release("v0.332.0", &[LINUX])];

        let selection = select_installable(&releases, Some(LINUX));

        assert_eq!(
            selection
                .installable
                .as_ref()
                .map(|entry| entry.tag.as_str()),
            Some("v0.333.0")
        );
        assert!(selection.upgrade_fallback_notice(LINUX).is_none());
        assert!(selection.skipped.is_empty());
    }

    /// A release whose archive shipped but whose checksum did not cannot be
    /// installed: the installer downloads both and verifies one against the
    /// other. Counting it as installable would reintroduce the 404 one step
    /// later, at the checksum fetch.
    #[test]
    fn an_archive_without_its_checksum_is_not_installable() {
        let mut partial = release("v0.333.0", &[]);
        partial.assets.push(ReleaseAsset {
            name: asset_archive_name(LINUX),
        });
        let releases = vec![partial, release("v0.332.0", &[LINUX])];

        let selection = select_installable(&releases, Some(LINUX));

        assert_eq!(
            selection
                .installable
                .as_ref()
                .map(|entry| entry.tag.as_str()),
            Some("v0.332.0")
        );
    }

    /// Nothing installable anywhere is a different finding from "an older
    /// release will do", and must not be dressed up as a fallback.
    #[test]
    fn no_installable_release_reports_none_without_inventing_a_fallback() {
        let releases = vec![release("v0.333.0", &[MAC]), release("v0.332.0", &[MAC])];

        let selection = select_installable(&releases, Some(LINUX));

        assert!(selection.installable.is_none());
        assert!(selection.skipped.is_empty());
        assert!(selection.upgrade_fallback_notice(LINUX).is_none());
        assert_eq!(
            selection.newest.map(|entry| entry.tag),
            Some("v0.333.0".to_string())
        );
    }

    /// Drafts and prereleases are not upgrade targets, and an incomparable tag
    /// cannot be ordered honestly, so neither may become `newest`.
    #[test]
    fn drafts_prereleases_and_unparseable_tags_are_excluded() {
        let mut draft = release("v0.334.0", &[LINUX]);
        draft.draft = true;
        let mut prerelease = release("v0.334.0-rc.1", &[LINUX]);
        prerelease.prerelease = true;
        let nonsense = release("nightly", &[LINUX]);
        let releases = vec![draft, prerelease, nonsense, release("v0.332.0", &[LINUX])];

        let selection = select_installable(&releases, Some(LINUX));

        assert_eq!(
            selection.newest.map(|entry| entry.tag),
            Some("v0.332.0".to_string())
        );
    }

    /// GitHub happens to return releases newest-first, but selection must not
    /// depend on that: an out-of-order page has to produce the same answer.
    #[test]
    fn selection_orders_by_semver_not_by_response_order() {
        let releases = vec![
            release("v0.331.0", &[LINUX]),
            release("v0.333.0", &[MAC]),
            release("v0.332.0", &[LINUX]),
        ];

        let selection = select_installable(&releases, Some(LINUX));

        assert_eq!(
            selection.newest.map(|entry| entry.tag),
            Some("v0.333.0".to_string())
        );
        assert_eq!(
            selection.installable.map(|entry| entry.tag),
            Some("v0.332.0".to_string())
        );
    }

    /// An undetermined target must not be silently treated as "no asset
    /// anywhere". Nothing was verified, so nothing is reported as skipped.
    #[test]
    fn an_undetermined_target_verifies_nothing_and_skips_nothing() {
        let releases = vec![release("v0.333.0", &[MAC]), release("v0.332.0", &[LINUX])];

        let selection = select_installable(&releases, None);

        assert_eq!(
            selection.installable.map(|entry| entry.tag),
            Some("v0.333.0".to_string())
        );
        assert!(selection.skipped.is_empty());
    }

    #[test]
    fn target_triple_detection_refuses_to_guess() {
        assert_eq!(
            target_triple_for("linux", "x86_64"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            target_triple_for("macos", "aarch64"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(target_triple_for("windows", "x86_64"), None);
        assert_eq!(target_triple_for("linux", "riscv64"), None);
    }

    #[test]
    fn pinned_lookup_accepts_both_tag_spellings() {
        let releases = vec![release("v0.333.0", &[MAC]), release("v0.332.0", &[LINUX])];

        for requested in ["v0.332.0", "0.332.0", "  v0.332.0  "] {
            let found = find_release(&releases, requested).expect("pinned release resolves");
            assert_eq!(found.tag, "v0.332.0");
            assert_eq!(found.version, "0.332.0");
        }

        assert!(find_release(&releases, "0.999.0").is_none());
    }

    /// Pinning is deliberate, so pinning a release that cannot install here is
    /// a distinct answer from pinning one that does not exist.
    #[test]
    fn pinned_installability_is_reported_separately_from_existence() {
        let releases = vec![release("v0.333.0", &[MAC]), release("v0.332.0", &[LINUX])];

        assert_eq!(
            release_installs_on(&releases, "v0.332.0", Some(LINUX)),
            Some(true)
        );
        assert_eq!(
            release_installs_on(&releases, "v0.333.0", Some(LINUX)),
            Some(false)
        );
        assert_eq!(
            release_installs_on(&releases, "v0.999.0", Some(LINUX)),
            None
        );
        assert_eq!(release_installs_on(&releases, "v0.332.0", None), None);
    }

    #[test]
    fn installable_tags_lists_newest_first_and_respects_the_limit() {
        let releases = vec![
            release("v0.333.0", &[MAC]),
            release("v0.332.0", &[LINUX]),
            release("v0.331.0", &[LINUX]),
            release("v0.330.0", &[LINUX]),
        ];

        assert_eq!(
            installable_tags(&releases, Some(LINUX), 2),
            vec!["v0.332.0".to_string(), "v0.331.0".to_string()]
        );
        assert!(installable_tags(&releases, None, 2).is_empty());
    }

    /// The URL in the failure hint has to be the URL the installer actually
    /// fetched, otherwise it sends the operator to the wrong place.
    #[test]
    fn asset_url_reflects_pinned_and_floating_resolution() {
        assert_eq!(
            asset_url(Some("v0.332.0"), LINUX),
            "https://github.com/Extra-Chill/homeboy/releases/download/v0.332.0/homeboy-x86_64-unknown-linux-gnu.tar.xz"
        );
        assert_eq!(
            asset_url(None, LINUX),
            "https://github.com/Extra-Chill/homeboy/releases/latest/download/homeboy-x86_64-unknown-linux-gnu.tar.xz"
        );
    }
}
