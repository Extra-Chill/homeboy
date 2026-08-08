//! Executable reconciliation coverage for the release asset completeness gate.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const TAG: &str = "v1.2.3";
const ASSETS: [&str; 3] = ["payload.tar.gz", "payload.tar.gz.sha256", "sha256.sum"];

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
    fs::write(artifacts.join("sha256.sum"), checksum).expect("write aggregate sidecar");

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
        // The helper uses GNU `sha256sum`; make the test portable to macOS.
        write_executable(
            &bin.join("sha256sum"),
            "#!/usr/bin/env bash\nshasum -a 256 \"$1\"\n",
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

    let fixture = Fixture::new();
    let mut duplicate: serde_json::Value =
        serde_json::from_str(&inventory(&fixture.digests, &[])).unwrap();
    let copy = duplicate["assets"][0].clone();
    duplicate["assets"].as_array_mut().unwrap().push(copy);
    assert!(!fixture.run(&[duplicate.to_string()]).status.success());
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

    for path in ["payload.tar.gz", "payload.tar.gz.sha256", "sha256.sum"] {
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
