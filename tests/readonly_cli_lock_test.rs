use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn read_only_cli_commands_complete_while_runtime_promotion_is_held() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let repository = create_repository(home.path());
        homeboy_core::observation::ObservationStore::open_initialized()
            .expect("initialize observation store");
        let promotion_namespace = home.path().join("nested-promotion-gate-data");
        let _promotion_namespace = TestPromotionNamespace::new(&promotion_namespace);
        let git_before = git_metadata_snapshot(&repository);
        let _promotion = homeboy::core::runtime_promotion::acquire("test promotion", "test")
            .expect("hold runtime promotion lease");
        let home = home.path();
        let home_before = filesystem_snapshot(home);
        let promotion_before = filesystem_snapshot(&promotion_namespace.join("runtime-promotion"));

        for args in [
            vec!["--version"],
            vec!["--help"],
            vec!["activity", "--help"],
            vec!["agent-task", "cook", "--help"],
            vec!["activity"],
            vec!["self", "identity"],
            vec!["self", "status"],
            vec!["status"],
            vec![
                "status",
                "--path",
                repository.to_str().expect("repository path"),
            ],
        ] {
            let output = run_with_timeout(&args, home, Duration::from_secs(10));
            assert!(
                output.status.success(),
                "{} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !output.stdout.is_empty(),
                "{} produced no diagnostic output",
                args.join(" ")
            );
            assert_snapshot_unchanged(
                &git_metadata_snapshot(&repository),
                &git_before,
                &format!(
                    "{} changed Git metadata while bypassing the mutation lock",
                    args.join(" ")
                ),
            );
            assert_snapshot_unchanged(
                &filesystem_snapshot(home),
                &home_before,
                &format!(
                    "{} changed config or runtime state while bypassing the mutation lock",
                    args.join(" ")
                ),
            );
        }

        let output = run_with_timeout(
            &[
                "status",
                "--path",
                repository.to_str().expect("repository path"),
                "--refresh",
            ],
            home,
            Duration::from_secs(10),
        );
        assert!(
            !output.status.success(),
            "refresh must serialize with the held promotion"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("runtime_promotion.contended"),
            "refresh serialization must report typed promotion contention"
        );
        assert_snapshot_unchanged(
            &git_metadata_snapshot(&repository),
            &git_before,
            "blocked refresh changed Git metadata",
        );
        assert_snapshot_unchanged(
            &filesystem_snapshot(&promotion_namespace.join("runtime-promotion")),
            &promotion_before,
            "blocked refresh changed the held promotion lease",
        );

        let output = run_with_timeout(
            &["upgrade", "--method", "binary"],
            home,
            Duration::from_secs(10),
        );
        assert!(
            !output.status.success(),
            "a concurrent mutation must not run"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("runtime_promotion.contended"),
            "mutation exclusion must report typed promotion contention: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_snapshot_unchanged(
            &git_metadata_snapshot(&repository),
            &git_before,
            "blocked upgrade changed Git metadata",
        );
        assert_snapshot_unchanged(
            &filesystem_snapshot(&promotion_namespace.join("runtime-promotion")),
            &promotion_before,
            "blocked upgrade changed the held promotion lease",
        );
    });
}

fn run_with_timeout(args: &[&str], home: &std::path::Path, timeout: Duration) -> Output {
    let child = Command::new(homeboy_bin())
        .args(args)
        .env("HOME", home)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .current_dir(home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Homeboy child");
    wait_for_output(child, timeout)
}

/// Isolate this test from a promotion gate's inherited data directory. Child
/// commands inherit this namespace from the test process just as Cargo does.
struct TestPromotionNamespace {
    previous: Option<OsString>,
}

impl TestPromotionNamespace {
    fn new(path: &std::path::Path) -> Self {
        let previous = std::env::var_os(homeboy_core::paths::HOMEBOY_DATA_DIR_ENV);
        std::env::set_var(homeboy_core::paths::HOMEBOY_DATA_DIR_ENV, path);
        Self { previous }
    }
}

impl Drop for TestPromotionNamespace {
    fn drop(&mut self) {
        match &self.previous {
            Some(path) => std::env::set_var(homeboy_core::paths::HOMEBOY_DATA_DIR_ENV, path),
            None => std::env::remove_var(homeboy_core::paths::HOMEBOY_DATA_DIR_ENV),
        }
    }
}

fn wait_for_output(mut child: Child, timeout: Duration) -> Output {
    let started = Instant::now();
    loop {
        if child.try_wait().expect("inspect Homeboy child").is_some() {
            return child
                .wait_with_output()
                .expect("collect Homeboy child output");
        }
        if started.elapsed() >= timeout {
            child.kill().expect("terminate blocked Homeboy child");
            let output = child
                .wait_with_output()
                .expect("collect timed-out Homeboy child");
            panic!("Homeboy child exceeded {timeout:?}: {output:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn create_repository(home: &std::path::Path) -> PathBuf {
    let repository = home.join("repository");
    fs::create_dir(&repository).expect("create test repository");
    for args in [
        vec!["init"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test User"],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repository)
            .status()
            .expect("configure test repository");
        assert!(status.success(), "configure test repository");
    }
    repository
}

/// Maximum characters a snapshot-mismatch message may occupy. A raw
/// `assert_eq!` on `BTreeMap<PathBuf, Vec<u8>>` prints every file as a decimal
/// byte vector, which has exceeded log and tool record limits and buried the
/// one changed path that mattered (#10633).
const SNAPSHOT_DIFF_MAX_CHARS: usize = 4_000;

type Snapshot = BTreeMap<PathBuf, Vec<u8>>;

/// Assert two filesystem snapshots match, reporting a bounded semantic diff —
/// added, removed and content-changed paths with size metadata — instead of raw
/// bytes.
#[track_caller]
fn assert_snapshot_unchanged(actual: &Snapshot, expected: &Snapshot, context: &str) {
    if actual == expected {
        return;
    }
    panic!("{context}\n{}", describe_snapshot_diff(expected, actual));
}

fn describe_snapshot_diff(before: &Snapshot, after: &Snapshot) -> String {
    let mut lines = Vec::new();

    for (path, bytes) in after {
        match before.get(path) {
            None => lines.push(format!(
                "  + added   {} ({} bytes)",
                path.display(),
                bytes.len()
            )),
            Some(previous) if previous != bytes => lines.push(format!(
                "  ~ changed {} ({} -> {} bytes)",
                path.display(),
                previous.len(),
                bytes.len()
            )),
            Some(_) => {}
        }
    }
    for (path, bytes) in before {
        if !after.contains_key(path) {
            lines.push(format!(
                "  - removed {} ({} bytes)",
                path.display(),
                bytes.len()
            ));
        }
    }

    if lines.is_empty() {
        return "  (snapshots differ but no per-path delta was derived)".to_string();
    }

    let total = lines.len();
    let mut rendered = String::from("snapshot delta:\n");
    let mut shown = 0;
    // Reserve room for the truncation notice up front, so appending it can never
    // push the message back over the bound it exists to enforce.
    let remainder_budget = format!("  … and {total} more path(s)\n").len();
    let line_budget = SNAPSHOT_DIFF_MAX_CHARS.saturating_sub(remainder_budget);
    for line in &lines {
        if rendered.len() + line.len() + 1 > line_budget {
            break;
        }
        rendered.push_str(line);
        rendered.push('\n');
        shown += 1;
    }
    if shown < total {
        rendered.push_str(&format!("  … and {} more path(s)\n", total - shown));
    }
    rendered
}

fn git_metadata_snapshot(repository: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
    filesystem_snapshot(&repository.join(".git"))
}

fn filesystem_snapshot(root: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut snapshot = BTreeMap::new();
    snapshot_directory(root, root, &mut snapshot);
    snapshot
}

fn snapshot_directory(
    root: &std::path::Path,
    directory: &std::path::Path,
    snapshot: &mut BTreeMap<PathBuf, Vec<u8>>,
) {
    for entry in fs::read_dir(directory).expect("read Git metadata") {
        let path = entry.expect("read Git metadata entry").path();
        if path.is_dir() {
            snapshot_directory(root, &path, snapshot);
        } else {
            snapshot.insert(
                path.strip_prefix(root)
                    .expect("relative Git metadata")
                    .into(),
                fs::read(&path).expect("read Git metadata file"),
            );
        }
    }
}

/// The #10621 delta: two added paths hidden inside a full byte-map dump. The
/// diff must name them directly (#10633).
#[test]
fn snapshot_diff_names_the_added_paths_from_the_10621_regression() {
    let before = Snapshot::new();
    let mut after = Snapshot::new();
    after.insert(
        PathBuf::from(".config/homeboy/deferred-workloads.json"),
        b"{}".to_vec(),
    );
    after.insert(
        PathBuf::from(".config/homeboy/deferred-workloads.lock"),
        Vec::new(),
    );

    let diff = describe_snapshot_diff(&before, &after);

    assert!(
        diff.contains(".config/homeboy/deferred-workloads.json"),
        "diff must name the added JSON record: {diff}"
    );
    assert!(
        diff.contains(".config/homeboy/deferred-workloads.lock"),
        "diff must name the added lock: {diff}"
    );
    assert!(
        diff.contains("+ added"),
        "added paths must be labelled: {diff}"
    );
}

/// A single oversized file must not reintroduce the unbounded byte dump.
#[test]
fn snapshot_diff_reports_sizes_not_bytes_and_stays_bounded() {
    let mut before = Snapshot::new();
    before.insert(PathBuf::from("big.bin"), vec![b'a'; 200_000]);
    let mut after = Snapshot::new();
    after.insert(PathBuf::from("big.bin"), vec![b'b'; 300_000]);

    let diff = describe_snapshot_diff(&before, &after);

    assert!(
        diff.len() <= SNAPSHOT_DIFF_MAX_CHARS,
        "diff must stay bounded"
    );
    assert!(
        diff.contains("200000 -> 300000 bytes"),
        "changed files report size metadata: {diff}"
    );
    assert!(
        !diff.contains("97, 97"),
        "raw byte vectors must never be rendered: {diff}"
    );
}

/// Many changed paths are truncated with an explicit remainder count rather
/// than being allowed to grow without limit.
#[test]
fn snapshot_diff_truncates_a_large_path_set_with_a_remainder_count() {
    let before = Snapshot::new();
    let mut after = Snapshot::new();
    for index in 0..500 {
        after.insert(
            PathBuf::from(format!(".config/homeboy/generated-path-{index:04}.json")),
            b"{}".to_vec(),
        );
    }

    let diff = describe_snapshot_diff(&before, &after);

    assert!(
        diff.len() <= SNAPSHOT_DIFF_MAX_CHARS,
        "diff must stay bounded"
    );
    assert!(
        diff.contains("more path(s)"),
        "truncation must be explicit: {diff}"
    );
}
