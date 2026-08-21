//! Executable reconciliation coverage for the release asset completeness gate.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const TAG: &str = "v1.2.3";

/// The full planned contract, as `release.yml` passes it in `EXPECTED_ASSETS`.
///
/// `dist-manifest.json` is deliberately part of this set (#12341). It is the
/// asset cargo-dist attaches during *announce*, so the pre-publication gate
/// runs with `REQUIRE_ANNOUNCE_ASSETS=false` and must not demand it — while
/// still permitting it, because recovery runs against releases that were
/// already announced and therefore already carry it.
///
/// Before #12341 this array held only the three payload/checksum entries, so
/// the announce strip the fixture exercises at `REQUIRE_ANNOUNCE_ASSETS=false`
/// removed nothing and the required/allowed conflation was invisible to every
/// test in this file.
///
/// The bootstrap assets are deliberately part of this set (#12701). cargo-dist
/// plans `homeboy-installer.sh` and the Homebrew formula but never lists them
/// in `sha256.sum` -- an installer cannot be meaningfully verified by a
/// checksum served from the same release. Before #12701 this array held only
/// payload/checksum entries, so a coverage assertion that demanded a sidecar
/// for every non-sidecar asset was vacuously satisfiable here and
/// unsatisfiable in production. Every release from v0.350.22 onward failed to
/// publish and not one test in this file noticed.
const ASSETS: [&str; 6] = [
    "payload.tar.gz",
    "payload.tar.gz.sha256",
    "sha256.sum",
    INSTALLER_ASSET,
    FORMULA_ASSET,
    ANNOUNCE_ASSET,
];

/// Attached during announce; required only at the published boundary.
const ANNOUNCE_ASSET: &str = "dist-manifest.json";

/// Planned, uploaded and digest-verified, but never checksum-covered.
const INSTALLER_ASSET: &str = "homeboy-installer.sh";
const FORMULA_ASSET: &str = "homeboy.rb";

/// What the pre-publication gate actually requires to be present and valid.
const REQUIRED_ASSETS: [&str; 5] = [
    "payload.tar.gz",
    "payload.tar.gz.sha256",
    "sha256.sum",
    INSTALLER_ASSET,
    FORMULA_ASSET,
];

fn digest(path: &Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("shasum must be available for release artifact fixtures");
    assert!(output.status.success());
    format!(
        "sha256:{}",
        String::from_utf8(output.stdout)
            .expect("shasum output is utf-8")
            .split_whitespace()
            .next()
            .expect("shasum emits a digest")
    )
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write mock executable");
    let mut permissions = fs::metadata(path).expect("mock metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("mark mock executable");
}

fn write_artifacts(root: &Path) -> BTreeMap<String, String> {
    let artifacts = root.join("artifacts");
    fs::create_dir(&artifacts).expect("create artifacts directory");
    let payload = artifacts.join("payload.tar.gz");
    fs::write(&payload, "rebuilt payload\n").expect("write payload");
    let payload_digest = digest(&payload).trim_start_matches("sha256:").to_owned();
    let checksum = format!("{payload_digest} *payload.tar.gz\n");
    fs::write(artifacts.join("payload.tar.gz.sha256"), &checksum).expect("write payload sidecar");
    // Note what is NOT here: the aggregate sidecar covers the archive only.
    // That mirrors cargo-dist, whose `sha256.sum` lists the platform archives
    // and `source.tar.gz` and nothing else (#12701).
    fs::write(artifacts.join("sha256.sum"), checksum).expect("write aggregate sidecar");
    fs::write(artifacts.join(INSTALLER_ASSET), "#!/bin/sh\necho install\n")
        .expect("write installer");
    fs::write(
        artifacts.join(FORMULA_ASSET),
        "class Homeboy < Formula\nend\n",
    )
    .expect("write formula");
    // `Preserve existing published asset bytes` copies every planned asset --
    // including the announce manifest -- into the recovery artifact directory.
    fs::write(
        artifacts.join(ANNOUNCE_ASSET),
        "{\"dist_version\":\"0.31.0\"}\n",
    )
    .expect("write announce manifest");

    ASSETS
        .into_iter()
        .map(|asset| {
            let path = artifacts.join(asset);
            (asset.to_owned(), digest(&path))
        })
        .collect()
}

fn inventory(digests: &BTreeMap<String, String>, overrides: &[(&str, &str, &str, u64)]) -> String {
    let overrides: BTreeMap<_, _> = overrides
        .iter()
        .map(|(name, state, remote_digest, size)| (*name, (*state, *remote_digest, *size)))
        .collect();
    let assets = ASSETS
        .into_iter()
        .filter_map(|name| {
            let (state, remote_digest, size) = overrides
                .get(name)
                .copied()
                .unwrap_or(("uploaded", digests[name].as_str(), 1));
            (state != "absent").then(|| {
                serde_json::json!({"name": name, "state": state, "size": size, "digest": remote_digest})
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({"isDraft": true, "assets": assets}).to_string()
}

struct Fixture {
    temp: TempDir,
    digests: BTreeMap<String, String>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("create test directory");
        let bin = temp.path().join("bin");
        let inventories = temp.path().join("inventories");
        fs::create_dir_all(&bin).expect("create mock bin");
        fs::create_dir(&inventories).expect("create inventories directory");
        write_executable(
            &bin.join("gh"),
            r#"#!/usr/bin/env bash
set -eu
case "$1 $2" in
  "release view")
    printf 'view\n' >> "$MOCK_GH_LOG"
    count_file="$MOCK_GH_INVENTORY_DIR/count"
    count=$(test -f "$count_file" && cat "$count_file" || printf 0)
    next=$((count + 1))
    printf '%s' "$next" > "$count_file"
    cat "$MOCK_GH_INVENTORY_DIR/$next.json"
    ;;
  "release upload")
    printf 'upload:%s\n' "${4##*/}" >> "$MOCK_GH_LOG"
    ;;
  *)
    printf 'unexpected:%s\n' "$*" >> "$MOCK_GH_LOG"
    exit 97
    ;;
esac
"#,
        );
        let digests = write_artifacts(temp.path());
        Self { temp, digests }
    }

    fn artifact_dir(&self) -> PathBuf {
        self.temp.path().join("artifacts")
    }

    fn run(&self, inventories: &[String]) -> Output {
        let inventory_dir = self.temp.path().join("inventories");
        for (index, inventory) in inventories.iter().enumerate() {
            fs::write(inventory_dir.join(format!("{}.json", index + 1)), inventory)
                .expect("write mock inventory");
        }
        let old_path = std::env::var("PATH").expect("PATH is set");
        Command::new("bash")
            .arg(".github/release-asset-completeness.sh")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("RELEASE_TAG", TAG)
            .env(
                "EXPECTED_ASSETS",
                serde_json::to_string(&ASSETS).expect("asset JSON"),
            )
            .env("REQUIRE_ANNOUNCE_ASSETS", "false")
            .env("ASSET_DIR", self.artifact_dir())
            .env("RECONCILE", "true")
            .env("MOCK_GH_LOG", self.temp.path().join("gh.log"))
            .env("MOCK_GH_INVENTORY_DIR", inventory_dir)
            .env(
                "PATH",
                format!("{}:{old_path}", self.temp.path().join("bin").display()),
            )
            .output()
            .expect("release asset helper should run")
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.temp.path().join("gh.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn retains_digest_matching_assets_without_uploading_or_publishing() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        inventory(&fixture.digests, &[]),
        inventory(&fixture.digests, &[]),
    ]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.calls(), ["view", "view"]);
}

/// Recovery against an already-announced, already-complete release is a no-op.
///
/// This is the exact shape that failed in production: v0.345.0 was published
/// complete with all 14 planned assets, and the recovery run rejected it with
/// "inventory has unexpected or duplicate assets" because the announce manifest
/// had been stripped out of the set being used as an allowlist (#12341).
#[test]
fn accepts_a_complete_announced_inventory_without_uploading_or_republishing() {
    let fixture = Fixture::new();
    let complete = inventory(&fixture.digests, &[]);
    assert!(
        complete.contains(ANNOUNCE_ASSET),
        "the fixture inventory must carry the announce asset for this to regress"
    );

    let output = fixture.run(&[complete.clone(), complete]);

    assert!(
        output.status.success(),
        "a complete announced inventory must reconcile cleanly.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.calls(), ["view", "view"]);
}

/// The announce asset is permitted, never demanded, before announce runs.
#[test]
fn accepts_an_inventory_without_the_announce_asset_before_announce() {
    let fixture = Fixture::new();
    let pre_announce = inventory(&fixture.digests, &[(ANNOUNCE_ASSET, "absent", "", 0)]);
    assert!(!pre_announce.contains(ANNOUNCE_ASSET));

    let output = fixture.run(&[pre_announce.clone(), pre_announce]);

    assert!(
        output.status.success(),
        "the announce asset must not be required before announce.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Never uploaded: it is not part of the rebuilt required contract.
    assert_eq!(fixture.calls(), ["view", "view"]);
}

/// Bootstrap assets are digest-verified but never checksum-covered (#12701).
///
/// cargo-dist plans `homeboy-installer.sh` and the Homebrew formula, and never
/// lists either in `sha256.sum`. A coverage assertion that demanded a sidecar
/// for every non-sidecar asset was therefore unsatisfiable in production: every
/// release from v0.350.22 onward failed publish with "No rebuilt checksum
/// contract covers homeboy-installer.sh" while the release itself published
/// complete with all 14 assets.
///
/// Both halves matter, so both are pinned here. Narrowing the coverage rule
/// must not narrow what gets verified: these assets still reconcile against
/// GitHub's digest of the rebuilt local bytes.
#[test]
fn bootstrap_assets_are_digest_verified_without_requiring_checksum_coverage() {
    let fixture = Fixture::new();
    let aggregate = fs::read_to_string(fixture.artifact_dir().join("sha256.sum"))
        .expect("read aggregate sidecar");
    for asset in [INSTALLER_ASSET, FORMULA_ASSET] {
        assert!(
            !aggregate.contains(asset),
            "{asset} must stay outside the checksum contract for this to regress"
        );
    }

    // Half one: uncovered bootstrap assets do not block a clean reconcile.
    let output = fixture.run(&[
        inventory(&fixture.digests, &[]),
        inventory(&fixture.digests, &[]),
    ]);
    assert!(
        output.status.success(),
        "assets cargo-dist never checksums must not be demanded to carry a checksum contract.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.calls(), ["view", "view"]);

    // Half two: their bytes are still proven against the remote digest.
    for asset in [INSTALLER_ASSET, FORMULA_ASSET] {
        let fixture = Fixture::new();
        let output = fixture.run(&[
            inventory(
                &fixture.digests,
                &[(
                    asset,
                    "uploaded",
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                    1,
                )],
            ),
            inventory(&fixture.digests, &[]),
        ]);
        assert!(
            output.status.success(),
            "{asset}: stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fixture.calls(),
            vec![
                "view".to_owned(),
                format!("upload:{asset}"),
                "view".to_owned()
            ],
            "{asset} must be replaced when the remote digest disagrees with the rebuilt bytes"
        );
    }
}

#[test]
fn replaces_missing_empty_and_digest_mismatched_assets_then_revalidates_remote_bytes() {
    for (asset, state, remote_digest, size) in [
        ("payload.tar.gz", "absent", "", 0),
        ("payload.tar.gz.sha256", "uploaded", "", 0),
        (
            "sha256.sum",
            "uploaded",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            1,
        ),
    ] {
        let fixture = Fixture::new();
        let output = fixture.run(&[
            inventory(&fixture.digests, &[(asset, state, remote_digest, size)]),
            inventory(&fixture.digests, &[]),
        ]);

        assert!(
            output.status.success(),
            "{asset}: stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fixture.calls(),
            vec![
                "view".to_owned(),
                format!("upload:{asset}"),
                "view".to_owned()
            ]
        );
    }
}

#[test]
fn rejects_malformed_unexpected_or_duplicate_remote_inventory_without_uploading() {
    let fixture = Fixture::new();
    assert!(!fixture.run(&["not JSON".to_owned()]).status.success());
    assert_eq!(fixture.calls(), ["view"]);

    let malformed = r#"{"assets":[{"name":42}]}"#.to_owned();
    let fixture = Fixture::new();
    assert!(!fixture.run(&[malformed]).status.success());
    assert_eq!(fixture.calls(), ["view"]);

    let fixture = Fixture::new();
    let unexpected =
        r#"{"assets":[{"name":"other","state":"uploaded","size":1,"digest":"sha256:abc"}]}"#
            .to_owned();
    assert!(!fixture.run(&[unexpected]).status.success());
    assert_eq!(fixture.calls(), ["view"]);

    // An unowned asset riding alongside an otherwise complete inventory must
    // still fail closed. Widening the allowlist to admit announce assets
    // (#12341) must not widen it to admit anything else.
    let fixture = Fixture::new();
    let mut intruder: serde_json::Value =
        serde_json::from_str(&inventory(&fixture.digests, &[])).unwrap();
    intruder["assets"].as_array_mut().unwrap().push(
        serde_json::json!({"name":"unowned.tar.xz","state":"uploaded","size":1,"digest":"sha256:abc"}),
    );
    assert!(!fixture.run(&[intruder.to_string()]).status.success());
    assert_eq!(fixture.calls(), ["view"]);

    let fixture = Fixture::new();
    let mut duplicate: serde_json::Value =
        serde_json::from_str(&inventory(&fixture.digests, &[])).unwrap();
    let copy = duplicate["assets"][0].clone();
    duplicate["assets"].as_array_mut().unwrap().push(copy);
    assert!(!fixture.run(&[duplicate.to_string()]).status.success());
    assert_eq!(fixture.calls(), ["view"]);

    // A duplicated announce asset is a duplicate like any other.
    let fixture = Fixture::new();
    let mut duplicate_announce: serde_json::Value =
        serde_json::from_str(&inventory(&fixture.digests, &[])).unwrap();
    let announce = duplicate_announce["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|asset| asset["name"] == ANNOUNCE_ASSET)
        .expect("fixture inventory carries the announce asset")
        .clone();
    duplicate_announce["assets"]
        .as_array_mut()
        .unwrap()
        .push(announce);
    assert!(!fixture
        .run(&[duplicate_announce.to_string()])
        .status
        .success());
    assert_eq!(fixture.calls(), ["view"]);
}

#[test]
fn rejects_invalid_local_artifacts_before_any_upload_or_publication() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.artifact_dir().join("payload.tar.gz")).expect("remove local artifact");
    assert!(!fixture
        .run(&[inventory(&fixture.digests, &[])])
        .status
        .success());
    assert_eq!(fixture.calls(), ["view"]);

    for path in REQUIRED_ASSETS {
        let fixture = Fixture::new();
        fs::write(fixture.artifact_dir().join(path), "").expect("empty local artifact");
        assert!(!fixture
            .run(&[inventory(&fixture.digests, &[])])
            .status
            .success());
        assert_eq!(fixture.calls(), ["view"]);
    }

    let fixture = Fixture::new();
    fs::write(
        fixture.artifact_dir().join("payload.tar.gz.sha256"),
        "not a checksum\n",
    )
    .expect("corrupt checksum sidecar");
    assert!(!fixture
        .run(&[inventory(&fixture.digests, &[])])
        .status
        .success());
    assert_eq!(fixture.calls(), ["view"]);
}

#[test]
fn rejects_checksum_sidecars_without_one_newline_terminated_record() {
    let fixture = Fixture::new();
    let sidecar = fixture.artifact_dir().join("payload.tar.gz.sha256");
    let contents = fs::read_to_string(&sidecar).expect("read payload sidecar");
    fs::write(&sidecar, contents.trim_end_matches('\n')).expect("remove final newline");
    assert!(!fixture
        .run(&[inventory(&fixture.digests, &[])])
        .status
        .success());
    assert_eq!(fixture.calls(), ["view"]);

    let fixture = Fixture::new();
    let sidecar = fixture.artifact_dir().join("payload.tar.gz.sha256");
    fs::write(
        &sidecar,
        format!("{}\n", fs::read_to_string(&sidecar).expect("read sidecar")),
    )
    .expect("append blank record");
    assert!(!fixture
        .run(&[inventory(&fixture.digests, &[])])
        .status
        .success());
    assert_eq!(fixture.calls(), ["view"]);
}

#[test]
fn rejects_multiple_checksum_records_before_reconciliation() {
    let fixture = Fixture::new();
    let sidecar = fixture.artifact_dir().join("payload.tar.gz.sha256");
    let contents = fs::read_to_string(&sidecar).expect("read payload sidecar");
    fs::write(&sidecar, format!("{contents}{contents}")).expect("append second checksum record");

    let output = fixture.run(&[inventory(&fixture.digests, &[])]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("must contain exactly one newline-terminated record"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(fixture.calls(), ["view"]);
}

#[test]
fn rejects_malformed_checksum_records_with_the_strict_installer_parser() {
    let fixture = Fixture::new();
    fs::write(
        fixture.artifact_dir().join("payload.tar.gz.sha256"),
        "not a checksum\n",
    )
    .expect("write malformed checksum record");

    let output = fixture.run(&[inventory(&fixture.digests, &[])]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("is rejected by the installer checksum parser"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(fixture.calls(), ["view"]);
}

#[test]
fn fails_when_post_upload_inventory_does_not_match_rebuilt_digests() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        inventory(&fixture.digests, &[("payload.tar.gz", "absent", "", 0)]),
        inventory(
            &fixture.digests,
            &[(
                "payload.tar.gz",
                "uploaded",
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                1,
            )],
        ),
    ]);

    assert!(!output.status.success());
    assert_eq!(fixture.calls(), ["view", "upload:payload.tar.gz", "view"]);
}
