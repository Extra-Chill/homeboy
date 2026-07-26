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
        let promotion_namespace = home.path().join("nested-promotion-gate-data");
        let _promotion_namespace = PromotionNamespaceGuard::new(&promotion_namespace);
        let git_before = git_metadata_snapshot(&repository);
        let _promotion = homeboy::core::runtime_promotion::acquire("test promotion", "test")
            .expect("hold runtime promotion lease");
        let home = home.path();
        let home_before = filesystem_snapshot(home);

        for args in [
            vec!["--version"],
            vec!["--help"],
            vec!["self", "identity"],
            vec!["self", "status"],
            vec!["status"],
            vec![
                "status",
                "--path",
                repository.to_str().expect("repository path"),
            ],
        ] {
            let output =
                run_with_timeout(&args, home, &promotion_namespace, Duration::from_secs(10));
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
            assert_eq!(
                git_metadata_snapshot(&repository),
                git_before,
                "{} changed Git metadata while bypassing the mutation lock",
                args.join(" ")
            );
            assert_eq!(
                filesystem_snapshot(home),
                home_before,
                "{} changed config or runtime state while bypassing the mutation lock",
                args.join(" ")
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
            &promotion_namespace,
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
        assert_eq!(
            git_metadata_snapshot(&repository),
            git_before,
            "blocked refresh changed Git metadata"
        );
        assert_eq!(
            filesystem_snapshot(home),
            home_before,
            "blocked refresh changed config or runtime state"
        );

        let output = run_with_timeout(
            &["upgrade", "--method", "binary"],
            home,
            &promotion_namespace,
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
    });
}

#[test]
fn production_promotion_lease_remains_visible_in_its_namespace() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let promotion_namespace = home.path().join("production-promotion-data");
        let _promotion_namespace = PromotionNamespaceGuard::new(&promotion_namespace);
        let _promotion = homeboy::core::runtime_promotion::acquire("production", "controller")
            .expect("hold production promotion lease");

        let output = run_with_timeout(
            &["status", "--refresh"],
            home.path(),
            &promotion_namespace,
            Duration::from_secs(10),
        );
        assert!(
            !output.status.success(),
            "production lease must block refresh"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("runtime_promotion.contended"),
            "production lease contention must remain typed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    });
}

fn run_with_timeout(
    args: &[&str],
    home: &std::path::Path,
    promotion_namespace: &std::path::Path,
    timeout: Duration,
) -> Output {
    let child = Command::new(homeboy_bin())
        .args(args)
        .env("HOME", home)
        .env("HOMEBOY_DATA_DIR", promotion_namespace)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .current_dir(home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Homeboy child");
    wait_for_output(child, timeout)
}

struct PromotionNamespaceGuard {
    previous: Option<OsString>,
}

impl PromotionNamespaceGuard {
    fn new(path: &std::path::Path) -> Self {
        let previous = std::env::var_os("HOMEBOY_DATA_DIR");
        std::env::set_var("HOMEBOY_DATA_DIR", path);
        Self { previous }
    }
}

impl Drop for PromotionNamespaceGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(path) => std::env::set_var("HOMEBOY_DATA_DIR", path),
            None => std::env::remove_var("HOMEBOY_DATA_DIR"),
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
