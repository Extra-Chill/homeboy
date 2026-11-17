//! Release-layout behavior, exercised through the real generated install
//! script (#18365). Every test runs the archive install locally.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use homeboy_core::engine::template::render_map;
use homeboy_core::server::SshClient;
use homeboy_extension_contract::{
    DeployArchiveInstallPolicy, DeployReleaseLayoutPolicy, DeployRequiredHeader, ExtensionManifest,
};

use crate::install_script::{self, Step};
use crate::test_support::local_client;
use crate::types::DeployResult;
use crate::version_overrides::{
    archive_install_override, archive_install_verification, deploy_override_template_vars,
};

const SLUG: &str = "component";

struct Fixture {
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::create_dir_all(temp.path().join("components")).expect("parent");
        fs::create_dir_all(temp.path().join("staging")).expect("staging");
        Self { temp }
    }

    fn parent(&self) -> PathBuf {
        self.temp.path().join("components")
    }

    fn target(&self) -> PathBuf {
        self.parent().join(SLUG)
    }

    fn staging(&self) -> PathBuf {
        self.temp.path().join("staging")
    }

    fn release_base(&self) -> PathBuf {
        self.parent().join(".homeboy-releases").join(SLUG)
    }

    /// Release directory names (not markers), sorted.
    fn releases(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.release_base())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| !name.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn active_release(&self) -> String {
        let link = fs::read_link(self.target()).expect("target is a symlink");
        let link = link.to_string_lossy().into_owned();
        let prefix = format!(".homeboy-releases/{SLUG}/");
        assert!(link.starts_with(&prefix), "relative managed link: {link}");
        let rest = &link[prefix.len()..];
        let (id, tail) = rest.split_once('/').expect("id/slug");
        assert_eq!(tail, SLUG, "release tree keeps the target basename");
        id.to_string()
    }

    /// A release archive whose main and lazily loaded class files both carry
    /// the release's contract version.
    fn artifact(&self, version: &str) -> PathBuf {
        let path = self.temp.path().join(format!("{SLUG}-{version}.zip"));
        let file = fs::File::create(&path).expect("zip");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default().unix_permissions(0o644);
        for (name, contents) in [
            (
                format!("{SLUG}/{SLUG}.manifest"),
                "Package Name: Fixture\n".to_string(),
            ),
            (format!("{SLUG}/main.txt"), version.to_string()),
            (format!("{SLUG}/inc/class.txt"), version.to_string()),
        ] {
            zip.start_file(name, options).expect("entry");
            zip.write_all(contents.as_bytes()).expect("contents");
        }
        zip.finish().expect("finish");
        path
    }

    fn policy(&self, layout: Option<DeployReleaseLayoutPolicy>) -> DeployArchiveInstallPolicy {
        DeployArchiveInstallPolicy {
            path_pattern: "/components/".to_string(),
            staging_path: self.staging().to_string_lossy().into_owned(),
            root_must_match_target_basename: true,
            required_header: Some(DeployRequiredHeader {
                file: Some("{{targetBasename}}.manifest".to_string()),
                file_glob: None,
                contains: "Package Name:".to_string(),
            }),
            skip_permissions_fix: false,
            after_switch_command: None,
            release_layout: layout,
        }
    }

    /// Full `deploy_artifact` path: upload, install, permissions, header
    /// verification through the target, cleanup.
    fn deploy(&self, version: &str, policy: &DeployArchiveInstallPolicy) -> DeployResult {
        self.deploy_with(&local_client(), version, policy)
    }

    fn deploy_with(
        &self,
        client: &SshClient,
        version: &str,
        policy: &DeployArchiveInstallPolicy,
    ) -> DeployResult {
        let artifact = self.artifact(version);
        let config = archive_install_override(policy);
        let verification = archive_install_verification(policy);
        let extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "id": "fixture", "name": "Fixture", "version": "1.0.0"
        }))
        .expect("extension");
        let install = crate::safety_and_artifact::OverrideInstall {
            config: &config,
            extension: &extension,
            site_root: Some(self.temp.path().to_str().expect("root")),
            domain: None,
            cli_path: None,
        };
        crate::safety_and_artifact::deploy_artifact(
            client,
            &artifact,
            self.target().to_str().expect("target"),
            &crate::safety_and_artifact::ArtifactInstall::Override(install),
            verification.as_ref(),
            None,
            None,
        )
        .expect("deploy result")
    }

    /// Render only the install command, staging `version` beside it.
    fn install_script(&self, version: &str, policy: &DeployArchiveInstallPolicy) -> String {
        let artifact = self.artifact(version);
        let staged = self.staging().join("artifact.zip");
        fs::copy(&artifact, &staged).expect("stage");
        let vars = deploy_override_template_vars(
            "artifact.zip",
            staged.to_str().expect("staged"),
            self.target().to_str().expect("target"),
            None,
            "wp",
            None,
        );
        render_map(&archive_install_override(policy).install_command, &vars)
    }
}

fn layout(precondition: Option<&str>) -> DeployReleaseLayoutPolicy {
    DeployReleaseLayoutPolicy {
        precondition_command: precondition.map(str::to_string),
        ..Default::default()
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

/// Whether this host can atomically exchange a symlink with a directory the
/// way the generated migration does.
fn host_supports_atomic_exchange() -> bool {
    let temp = tempfile::tempdir().expect("probe dir");
    fs::create_dir(temp.path().join("dir")).expect("dir");
    std::os::unix::fs::symlink("elsewhere", temp.path().join("link")).expect("link");
    let script = format!(
        "cd {dir} && {{ mv -T --exchange link dir 2>/dev/null || python3 -c {py} link dir 2>/dev/null; }}",
        dir = homeboy_core::engine::shell::quote_path(temp.path().to_str().expect("probe")),
        py = homeboy_core::engine::shell::quote_arg(crate::version_overrides::RENAMEAT2_EXCHANGE_PY),
    );
    std::process::Command::new("sh")
        .args(["-c", &script])
        .status()
        .is_ok_and(|status| status.success())
        && is_symlink(&temp.path().join("dir"))
}

/// A bin dir with the tools the install script needs, a `mv` that rejects
/// `--exchange` (and optionally `-T`), and no python3 — so neither atomic
/// exchange nor the python rename fallback exists.
fn restricted_bin(dir: &Path, reject_mv_t: bool) -> PathBuf {
    let bin = dir.join("restricted-bin");
    fs::create_dir_all(&bin).expect("bin");
    for tool in [
        "unzip", "awk", "mktemp", "rm", "mkdir", "basename", "ln", "readlink", "cat", "tail", "tr",
        "cksum", "date", "touch", "stat", "sort", "rmdir",
    ] {
        let resolved = ["/usr/bin", "/bin", "/usr/local/bin"]
            .iter()
            .map(|prefix| Path::new(prefix).join(tool))
            .find(|candidate| candidate.exists())
            .unwrap_or_else(|| panic!("cannot resolve {tool}"));
        std::os::unix::fs::symlink(resolved, bin.join(tool)).expect("tool");
    }
    let reject_t = if reject_mv_t {
        "case \"$1\" in -fT|-T) exit 65;; esac\n"
    } else {
        ""
    };
    homeboy_test_support::write_executable(
        bin.join("mv"),
        format!("#!/bin/sh\ncase \"$*\" in *--exchange*) exit 64;; esac\n{reject_t}exec /bin/mv \"$@\"\n"),
        0o755,
    )
    .expect("mv stub");
    bin
}

/// Run a rendered install command through the real step runner with PATH
/// restricted to `bin`, collecting warnings like a deploy does.
fn run_restricted(script: &str, bin: &Path) -> (Option<DeployResult>, Vec<String>) {
    let client = SshClient {
        env: HashMap::from([("PATH".to_string(), bin.to_string_lossy().into_owned())]),
        ..local_client()
    };
    let mut warnings = Vec::new();
    let steps = [Step::new(script.to_string(), |output| {
        Ok(DeployResult::failure(
            output.exit_code,
            format!("{}{}", output.stdout, output.stderr),
        ))
    })];
    let outcome =
        install_script::run_collecting_warnings(&client, &steps, &mut warnings).expect("run");
    (outcome, warnings)
}

// ---------------------------------------------------------------------------
// Acceptance (#18365): concurrent readers never see a mixed release.
// ---------------------------------------------------------------------------

/// What a PHP request does: resolve the component's real directory once
/// (`__DIR__` is the realpath, like `readlink -f`), include the main file,
/// then lazily autoload a class file from that same resolved directory.
fn request(target: &Path) -> Result<(String, String), &'static str> {
    let resolved = fs::canonicalize(target).map_err(|_| "target missing")?;
    let main = fs::read_to_string(resolved.join("main.txt")).map_err(|_| "main missing")?;
    std::thread::sleep(Duration::from_micros(300));
    let class = fs::read_to_string(resolved.join("inc/class.txt")).map_err(|_| "class missing")?;
    Ok((main, class))
}

#[test]
fn concurrent_readers_never_see_a_mixed_release_or_a_missing_target() {
    let fixture = Fixture::new();
    let policy = fixture.policy(Some(layout(Some("exit 0"))));
    let first = fixture.deploy("v1", &policy);
    assert!(first.success, "{:?}", first.error);

    let stop = Arc::new(AtomicBool::new(false));
    let mixed = Arc::new(Mutex::new(Vec::new()));
    let missing = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (stop, mixed, missing, requests, seen) = (
                Arc::clone(&stop),
                Arc::clone(&mixed),
                Arc::clone(&missing),
                Arc::clone(&requests),
                Arc::clone(&seen),
            );
            let target = fixture.target();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    requests.fetch_add(1, Ordering::Relaxed);
                    match request(&target) {
                        Ok((main, class)) if main == class => {
                            seen.lock().unwrap().insert(main);
                        }
                        Ok(pair) => mixed.lock().unwrap().push(pair),
                        Err(problem) => missing.lock().unwrap().push(problem),
                    }
                }
            })
        })
        .collect();

    for version in ["v2", "v3", "v4", "v5"] {
        let result = fixture.deploy(version, &policy);
        assert!(result.success, "{version}: {:?}", result.error);
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().expect("reader");
    }

    assert!(
        mixed.lock().unwrap().is_empty(),
        "a request mixed releases: {:?}",
        mixed.lock().unwrap()
    );
    assert!(
        missing.lock().unwrap().is_empty(),
        "a request saw a missing target or file: {:?}",
        missing.lock().unwrap()
    );
    let seen = seen.lock().unwrap();
    assert!(
        seen.len() >= 2,
        "readers must span several releases to mean anything: saw {seen:?} over {} requests",
        requests.load(Ordering::Relaxed)
    );
    assert_eq!(read(&fixture.target().join("main.txt")), "v5");
}

/// Why the acceptance test is meaningful: the same request shape, straddling
/// one deploy, reads a mixed pair under the in-place exchange flow and a
/// consistent pair under the release layout. Deterministic: the deploy runs
/// between the request's two reads.
#[test]
fn a_request_straddling_a_deploy_mixes_in_place_but_not_with_release_layout() {
    for (release_layout, expect_mixed) in [(None, true), (Some(layout(None)), false)] {
        let fixture = Fixture::new();
        let policy = fixture.policy(release_layout);
        assert!(fixture.deploy("v1", &policy).success);

        let resolved = fs::canonicalize(fixture.target()).expect("resolve");
        let main = read(&resolved.join("main.txt"));
        assert!(fixture.deploy("v2", &policy).success);
        let class = read(&resolved.join("inc/class.txt"));

        assert_eq!(main, "v1");
        if expect_mixed {
            assert_eq!(class, "v2", "in-place exchange: same path, new contents");
        } else {
            assert_eq!(
                class, "v1",
                "release layout: the resolved release is untouched"
            );
            assert_eq!(read(&fixture.target().join("inc/class.txt")), "v2");
        }
    }
}

// ---------------------------------------------------------------------------
// Layout, migration, precondition, flip failure.
// ---------------------------------------------------------------------------

#[test]
fn first_install_creates_a_relative_release_symlink_with_permissions_and_verification() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let policy = fixture.policy(Some(layout(Some("echo runtime-ok"))));
    let result = fixture.deploy("v1", &policy);
    assert!(result.success, "{:?}", result.error);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert!(
        result.effect.as_ref().expect("effect").verified,
        "header verification reads through the symlink"
    );

    assert!(is_symlink(&fixture.target()));
    let id = fixture.active_release();
    assert_eq!(fixture.releases(), vec![id.clone()]);
    let tree = fixture.release_base().join(&id).join(SLUG);
    assert_eq!(read(&tree.join("main.txt")), "v1");
    // Remote permission defaults add group write; they must reach the
    // release tree, not stop at the symlink.
    let mode = fs::metadata(tree.join("main.txt"))
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o020,
        0o020,
        "permissions applied to the release: {mode:o}"
    );
    assert!(
        fs::read_dir(fixture.parent())
            .expect("parent")
            .flatten()
            .all(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name == SLUG || name == ".homeboy-releases"
            }),
        "no temp links, staging, or backups leak beside the target"
    );
}

#[test]
fn migration_from_a_real_directory_is_atomic_and_keeps_the_old_tree() {
    if !host_supports_atomic_exchange() {
        eprintln!("SKIP: no atomic exchange on this host (fallback covered separately)");
        return;
    }
    let fixture = Fixture::new();
    // A pre-layout install: a real directory.
    assert!(fixture.deploy("v0", &fixture.policy(None)).success);
    assert!(!is_symlink(&fixture.target()));

    let result = fixture.deploy("v1", &fixture.policy(Some(layout(Some("true")))));
    assert!(result.success, "{:?}", result.error);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert!(
        is_symlink(&fixture.target()),
        "target is a symlink after migration"
    );
    let active = fixture.active_release();
    assert_eq!(read(&fixture.target().join("main.txt")), "v1");

    let migrated = format!("{active}-migrated");
    assert_eq!(fixture.releases(), {
        let mut expected = vec![active.clone(), migrated.clone()];
        expected.sort();
        expected
    });
    let old_tree = fixture.release_base().join(&migrated).join(SLUG);
    assert_eq!(
        read(&old_tree.join("main.txt")),
        "v0",
        "old tree kept as previous release"
    );
    assert!(
        fixture
            .release_base()
            .join(format!(".deactivated-{migrated}"))
            .exists(),
        "deactivation is recorded for retention"
    );

    // The next deploy is a plain flip; both previous releases are retained.
    let result = fixture.deploy("v2", &fixture.policy(Some(layout(Some("true")))));
    assert!(result.success, "{:?}", result.error);
    assert_eq!(read(&fixture.target().join("main.txt")), "v2");
    assert_eq!(fixture.releases().len(), 3);
}

/// The migration exchanges a symlink with a live directory concurrently with
/// readers: the target path must never be missing.
#[test]
fn migration_never_exposes_a_missing_target() {
    if !host_supports_atomic_exchange() {
        return;
    }
    let fixture = Fixture::new();
    assert!(fixture.deploy("v0", &fixture.policy(None)).success);
    let stop = Arc::new(AtomicBool::new(false));
    let missing = Arc::new(AtomicUsize::new(0));
    let reader = {
        let (stop, missing, target) = (Arc::clone(&stop), Arc::clone(&missing), fixture.target());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if !target.join("main.txt").exists() {
                    missing.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
    };
    let result = fixture.deploy("v1", &fixture.policy(Some(layout(None))));
    stop.store(true, Ordering::Relaxed);
    reader.join().expect("reader");
    assert!(result.success, "{:?}", result.error);
    assert!(is_symlink(&fixture.target()));
    assert_eq!(missing.load(Ordering::Relaxed), 0);
}

#[test]
fn migration_without_atomic_exchange_falls_back_in_place_with_a_warning() {
    let fixture = Fixture::new();
    assert!(fixture.deploy("v0", &fixture.policy(None)).success);
    let bin = restricted_bin(fixture.temp.path(), false);
    let script = fixture.install_script("v1", &fixture.policy(Some(layout(None))));

    let (failure, warnings) = run_restricted(&script, &bin);
    assert!(failure.is_none(), "{:?}", failure.and_then(|f| f.error));
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("needs an atomic exchange")),
        "{warnings:?}"
    );
    assert!(
        !is_symlink(&fixture.target()),
        "no migration without an atomic exchange"
    );
    assert_eq!(read(&fixture.target().join("main.txt")), "v1");
    assert!(
        !fixture.parent().join(".homeboy-releases").exists(),
        "the unused release directory is discarded"
    );
}

#[test]
fn precondition_failure_falls_back_to_in_place_with_a_warning_and_evidence() {
    let fixture = Fixture::new();
    let mut policy = fixture.policy(Some(layout(Some(
        "echo 'runtime: revalidate_path=off on /run/fpm.sock'\necho detail >&2\nexit 3",
    ))));
    let refreshed = fixture.temp.path().join("refreshed");
    policy.after_switch_command = Some(format!("touch '{}'", refreshed.display()));

    let result = fixture.deploy("v1", &policy);
    assert!(result.success, "{:?}", result.error);
    assert!(
        !is_symlink(&fixture.target()),
        "in-place flow publishes a real directory"
    );
    assert_eq!(read(&fixture.target().join("main.txt")), "v1");
    assert!(
        refreshed.exists(),
        "the in-place flow still runs after_switch_command"
    );
    assert!(!fixture.parent().join(".homeboy-releases").exists());
    assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    let warning = &result.warnings[0];
    assert!(
        warning.contains("precondition failed (exit 3)"),
        "{warning}"
    );
    assert!(
        warning.contains("revalidate_path=off"),
        "command output is evidence: {warning}"
    );
    assert!(
        warning.contains("detail"),
        "stderr is evidence too: {warning}"
    );
}

/// When a previously enabled layout later fails its precondition, the in-place
/// flow replaces the managed symlink with a real directory and leaves the
/// retained releases alone.
#[test]
fn precondition_failure_after_layout_was_active_publishes_a_real_directory() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .deploy("v1", &fixture.policy(Some(layout(None))))
            .success
    );
    let old = fixture.active_release();

    let result = fixture.deploy("v2", &fixture.policy(Some(layout(Some("exit 1")))));
    assert!(result.success, "{:?}", result.error);
    assert!(!result.warnings.is_empty());
    assert!(!is_symlink(&fixture.target()));
    assert_eq!(read(&fixture.target().join("main.txt")), "v2");
    assert_eq!(fixture.releases(), vec![old.clone()]);
    assert_eq!(
        read(
            &fixture
                .release_base()
                .join(&old)
                .join(SLUG)
                .join("main.txt")
        ),
        "v1"
    );
}

#[test]
fn flip_failure_keeps_the_previous_release_and_discards_the_new_one() {
    let fixture = Fixture::new();
    let policy = fixture.policy(Some(layout(None)));
    assert!(fixture.deploy("v1", &policy).success);
    let previous = fixture.active_release();

    // `mv -T` and python3 both unavailable: the rename over the link fails.
    let bin = restricted_bin(fixture.temp.path(), true);
    let script = fixture.install_script("v2", &policy);
    let (failure, _) = run_restricted(&script, &bin);
    let failure = failure.expect("flip failure fails the install");
    assert!(
        failure
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("flip failed"),
        "{:?}",
        failure.error
    );

    assert_eq!(
        fixture.active_release(),
        previous,
        "target still points at the previous release"
    );
    assert_eq!(read(&fixture.target().join("main.txt")), "v1");
    assert_eq!(
        fixture.releases(),
        vec![previous],
        "the new release is removed"
    );
    let leaked: Vec<_> = fs::read_dir(fixture.parent())
        .expect("parent")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".homeboy-link.") || name.starts_with(".homeboy-install."))
        .collect();
    assert!(leaked.is_empty(), "{leaked:?}");
}

#[test]
fn unmanaged_symlink_target_is_flipped_without_touching_its_destination() {
    let fixture = Fixture::new();
    let workspace = fixture.temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::write(workspace.join("main.txt"), "workspace").expect("file");
    std::os::unix::fs::symlink(&workspace, fixture.target()).expect("link");

    let result = fixture.deploy("v1", &fixture.policy(Some(layout(None))));
    assert!(result.success, "{:?}", result.error);
    assert!(result
        .warnings
        .iter()
        .any(|w| w.contains("unmanaged symlink")));
    fixture.active_release();
    assert_eq!(read(&fixture.target().join("main.txt")), "v1");
    assert_eq!(read(&workspace.join("main.txt")), "workspace");
}

#[test]
fn invalid_release_root_disables_the_layout_with_a_warning() {
    let fixture = Fixture::new();
    let result = fixture.deploy(
        "v1",
        &fixture.policy(Some(DeployReleaseLayoutPolicy {
            release_root_dir: "../escape".to_string(),
            ..Default::default()
        })),
    );
    assert!(result.success, "{:?}", result.error);
    assert!(result
        .warnings
        .iter()
        .any(|w| w.contains("Release layout disabled")));
    assert!(!is_symlink(&fixture.target()));
}

// ---------------------------------------------------------------------------
// Garbage collection.
// ---------------------------------------------------------------------------

#[test]
fn gc_keeps_active_plus_newest_previous_after_deploys() {
    let fixture = Fixture::new();
    let policy = fixture.policy(Some(DeployReleaseLayoutPolicy {
        keep_previous: 2,
        keep_recent_minutes: 0,
        ..Default::default()
    }));
    let mut activated = Vec::new();
    for version in ["v1", "v2", "v3", "v4", "v5"] {
        let result = fixture.deploy(version, &policy);
        assert!(result.success, "{version}: {:?}", result.error);
        activated.push(fixture.active_release());
        // Distinct deactivation stamps (second resolution).
        std::thread::sleep(Duration::from_millis(1100));
    }
    let mut expected = activated[2..].to_vec();
    expected.sort();
    assert_eq!(fixture.releases(), expected, "active + 2 newest previous");
    assert_eq!(fixture.active_release(), activated[4]);
    let markers: Vec<_> = fs::read_dir(fixture.release_base())
        .expect("base")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".deactivated-"))
        .collect();
    assert_eq!(
        markers.len(),
        2,
        "markers of pruned releases are removed: {markers:?}"
    );
}

#[test]
fn gc_keeps_recently_deactivated_releases_beyond_the_count() {
    let fixture = Fixture::new();
    let policy = fixture.policy(Some(DeployReleaseLayoutPolicy {
        keep_previous: 0,
        keep_recent_minutes: 15,
        ..Default::default()
    }));
    for version in ["v1", "v2", "v3", "v4"] {
        assert!(fixture.deploy(version, &policy).success);
    }
    assert_eq!(
        fixture.releases().len(),
        4,
        "all deactivated within 15 minutes"
    );
}

/// Exercise the GC fragment directly against hand-aged releases.
fn run_gc(fixture: &Fixture, keep_previous: u32, keep_recent_minutes: u32, just_activated: &str) {
    let gc = super::garbage_collect(&DeployReleaseLayoutPolicy {
        keep_previous,
        keep_recent_minutes,
        ..Default::default()
    });
    let script = format!(
        "target='{}'; release_base='{}'; release_rel_prefix='.homeboy-releases/{SLUG}'; release_id='{just_activated}'\n{gc}",
        fixture.target().display(),
        fixture.release_base().display(),
    );
    let output = std::process::Command::new("sh")
        .args(["-c", &script])
        .output()
        .expect("gc");
    assert!(output.status.success(), "{output:?}");
}

fn make_release(
    fixture: &Fixture,
    id: &str,
    deactivated_minutes_ago: Option<u64>,
    dir_age_minutes: u64,
) {
    let dir = fixture.release_base().join(id);
    fs::create_dir_all(dir.join(SLUG)).expect("release");
    fs::write(dir.join(SLUG).join("main.txt"), id).expect("file");
    let age = |minutes: u64| SystemTime::now() - Duration::from_secs(minutes * 60);
    fs::File::open(&dir)
        .expect("dir")
        .set_modified(age(dir_age_minutes))
        .expect("dir mtime");
    if let Some(minutes) = deactivated_minutes_ago {
        let marker = fixture.release_base().join(format!(".deactivated-{id}"));
        fs::write(&marker, "").expect("marker");
        fs::File::options()
            .write(true)
            .open(&marker)
            .expect("marker")
            .set_modified(age(minutes))
            .expect("marker mtime");
    }
}

fn activate(fixture: &Fixture, id: &str) {
    let _ = fs::remove_file(fixture.target());
    std::os::unix::fs::symlink(
        format!(".homeboy-releases/{SLUG}/{id}/{SLUG}"),
        fixture.target(),
    )
    .expect("activate");
}

#[test]
fn gc_applies_count_and_age_rules_and_never_deletes_the_active_release() {
    let fixture = Fixture::new();
    make_release(&fixture, "r5-active", None, 600);
    make_release(&fixture, "r4", Some(1), 600); // newest previous, recent
    make_release(&fixture, "r3", Some(10), 600); // 2nd previous, recent
    make_release(&fixture, "r2", Some(30), 600); // old and beyond the count
    make_release(&fixture, "r1", Some(60), 600); // old and beyond the count
    make_release(&fixture, "r0", None, 120); // never activated, old
    make_release(&fixture, "incoming", None, 0); // never activated, fresh
    activate(&fixture, "r5-active");

    run_gc(&fixture, 1, 15, "r5-active");
    assert_eq!(
        fixture.releases(),
        vec!["incoming", "r3", "r4", "r5-active"],
        "r4 by count; r3 and incoming by age; r2, r1, r0 pruned"
    );
    assert!(!fixture.release_base().join(".deactivated-r2").exists());

    // Zero retention: everything but the active release goes, even though the
    // active release is old and the GC was told another id was just activated.
    run_gc(&fixture, 0, 0, "unrelated");
    assert_eq!(fixture.releases(), vec!["r5-active"]);
    assert_eq!(read(&fixture.target().join("main.txt")), "r5-active");
}

#[test]
fn gc_failure_never_fails_the_deploy() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let policy = fixture.policy(Some(DeployReleaseLayoutPolicy {
        keep_previous: 0,
        keep_recent_minutes: 0,
        ..Default::default()
    }));
    assert!(fixture.deploy("v1", &policy).success);
    // An old previous release GC cannot remove: its tree is read-only.
    make_release(&fixture, "stuck", Some(60), 60);
    let stuck = fixture.release_base().join("stuck").join(SLUG);
    fs::set_permissions(&stuck, fs::Permissions::from_mode(0o555)).expect("chmod");
    let result = fixture.deploy("v2", &policy);
    let _ = fs::set_permissions(&stuck, fs::Permissions::from_mode(0o755));
    assert!(result.success, "{:?}", result.error);
    assert_eq!(read(&fixture.target().join("main.txt")), "v2");
    assert_eq!(fixture.releases().len(), 2, "active + the unremovable one");
}

// ---------------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------------

#[test]
fn rendered_layout_script_is_valid_shell_and_keeps_in_place_fallback() {
    let fixture = Fixture::new();
    let script = fixture.install_script("v1", &fixture.policy(Some(layout(Some("exit 0")))));
    let output = std::process::Command::new("sh")
        .args(["-n", "-c", &script])
        .output()
        .expect("sh -n");
    assert!(output.status.success(), "{output:?}");
    for needle in [
        "ln -s \"$release_link\" \"$link_tmp\"",
        "mv -fT \"$link_tmp\" \"$target\"",
        "mv -T --exchange \"$link_tmp\" \"$target\"",
        "mv -T --exchange \"$extracted\" \"$target\"",
    ] {
        assert!(script.contains(needle), "{needle}");
    }
    assert!(
        !script.contains("rm -f \"$target\"") && !script.contains("rm -rf \"$target\" && ln"),
        "the flip never removes then recreates the target"
    );
}

/// The shipped WordPress policies opt in to the layout, and their FPM
/// precondition fails closed: with no PHP-FPM socket the deploy installs in
/// place, warns, and still runs the inline opcache refresh.
#[test]
fn wordpress_policies_fail_closed_without_an_fpm_socket() {
    if std::process::Command::new("php")
        .arg("-v")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("SKIP: php CLI not available");
        return;
    }
    let manifest: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../extensions/wordpress/wordpress.json"
    ))
    .expect("wordpress.json");
    let mut policy: DeployArchiveInstallPolicy =
        serde_json::from_value(manifest["deploy"]["archive_install"][0].clone()).expect("policy");
    assert!(policy.release_layout.is_some(), "WordPress plugins opt in");

    let fixture = Fixture::new();
    policy.staging_path = fixture.staging().to_string_lossy().into_owned();
    policy.required_header = None;
    let client = SshClient {
        env: HashMap::from([("HOMEBOY_PHP_FPM_SOCKET".to_string(), " , ".to_string())]),
        ..local_client()
    };
    let result = fixture.deploy_with(&client, "v1", &policy);
    assert!(result.success, "{:?}", result.error);
    assert!(!is_symlink(&fixture.target()));
    assert_eq!(read(&fixture.target().join("main.txt")), "v1");
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("precondition failed") && w.contains("no writable PHP-FPM socket")),
        "{:?}",
        result.warnings
    );
}
