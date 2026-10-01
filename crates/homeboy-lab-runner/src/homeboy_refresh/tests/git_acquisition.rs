use super::*;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    _root: tempfile::TempDir,
    remote: PathBuf,
    old: String,
    new: String,
    noise_commit: String,
    noise_blob: String,
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git output");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("fixture root");
    let work = root.path().join("work");
    fs::create_dir(&work).expect("work");
    git(&work, &["init", "--quiet", "--initial-branch=main"]);
    git(&work, &["config", "user.email", "homeboy@example.test"]);
    git(&work, &["config", "user.name", "Homeboy Test"]);
    fs::write(work.join("release"), "old\n").expect("old");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "old"]);
    let old = git_output(&work, &["rev-parse", "HEAD"]);
    fs::write(work.join("release"), "new\n").expect("new");
    git(&work, &["commit", "-qam", "new"]);
    let new = git_output(&work, &["rev-parse", "HEAD"]);
    git(&work, &["checkout", "-q", "-b", "noise"]);
    fs::write(
        work.join("unrelated-branch-marker"),
        "unrelated-branch-object\n",
    )
    .expect("noise");
    git(&work, &["add", "unrelated-branch-marker"]);
    git(&work, &["commit", "-qm", "noise"]);
    let noise_commit = git_output(&work, &["rev-parse", "HEAD"]);
    let noise_blob = git_output(&work, &["rev-parse", "HEAD:unrelated-branch-marker"]);
    git(&work, &["checkout", "-q", "main"]);
    let remote = root.path().join("homeboy.git");
    git(
        &work,
        &[
            "init",
            "--bare",
            "--quiet",
            remote.to_str().expect("remote"),
        ],
    );
    git(
        &work,
        &[
            "push",
            "--quiet",
            remote.to_str().expect("remote"),
            "main",
            "noise",
        ],
    );
    git(
        root.path(),
        &[
            "--git-dir",
            remote.to_str().expect("remote"),
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
    );
    Fixture {
        _root: root,
        remote,
        old,
        new,
        noise_commit,
        noise_blob,
    }
}

fn run_preflight(
    source: &Path,
    git_ref: &str,
    dir: &Path,
    authorities: &[&str],
    env: &[(&str, &str)],
    path_prefix: Option<&Path>,
) -> std::process::Output {
    let script = materialize_script(
        source.to_str().expect("source"),
        git_ref,
        dir.to_str().expect("dir"),
        dir.join("target/release/homeboy").to_str().expect("binary"),
        false,
        authorities,
    );
    assert!(
        !script.contains("refs/heads/*"),
        "validation must not fetch every branch"
    );
    let guard = script
        .split_once("mkdir -p \"$(dirname \"$dir\")\"")
        .expect("preflight boundary")
        .0;
    let mut command = Command::new("bash");
    command.args(["-c", guard]);
    for (key, value) in env {
        command.env(key, value);
    }
    if let Some(prefix) = path_prefix {
        let path = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{}:{}", prefix.display(), path));
    }
    command.output().expect("run preflight")
}

fn cache_dir(checkout: &Path) -> PathBuf {
    checkout.join(".git")
}

fn object_absent(cache: &Path, object: &str) -> bool {
    !Command::new("git")
        .args([
            "--git-dir",
            cache.to_str().expect("cache"),
            "cat-file",
            "-e",
            object,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("cat-file")
        .success()
}

fn write_git_wrapper(dir: &Path, stall: bool, log: &Path) {
    let real = which_git();
    let script = format!(
        "#!/bin/sh\nlog={}\nreal={}\nstall={}\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    fetch|ls-remote)\n      printf '%s\\n' \"$*\" >> \"$log\"\n      if [ \"$stall\" = 1 ]; then sleep 30; exit 1; fi\n      echo \"unexpected network: $*\" >> \"$log\"\n      exit 97\n      ;;\n  esac\ndone\nexec \"$real\" \"$@\"\n",
        shell_quote(log),
        shell_quote(&real),
        if stall { "1" } else { "0" },
    );
    let path = dir.join("git");
    fs::write(&path, script).expect("wrapper");
    let _ = Command::new("chmod")
        .args(["0755", path.to_str().expect("wrapper path")])
        .status();
}

fn which_git() -> PathBuf {
    let output = Command::new("which")
        .arg("git")
        .output()
        .expect("which git");
    PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string(),
    )
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[test]
fn cached_identical_authorities_do_not_fetch() {
    let fixture = fixture();
    let checkout = tempfile::tempdir().expect("checkout");
    let dir = checkout.path().join("build");
    let first = run_preflight(
        &fixture.remote,
        &fixture.new,
        &dir,
        &[&fixture.new],
        &[],
        None,
    );
    assert!(
        first.status.success(),
        "seed cache: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let tools = tempfile::tempdir().expect("tools");
    let log = tools.path().join("network.log");
    write_git_wrapper(tools.path(), false, &log);
    let second = run_preflight(
        &fixture.remote,
        &fixture.new,
        &dir,
        &[&fixture.new, &fixture.new],
        &[],
        Some(tools.path()),
    );
    assert!(
        second.status.success(),
        "cached identical validation failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let network = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        network.is_empty(),
        "identical cached identities must not contact the network: {network}"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("status=cached_identical"));
    assert!(object_absent(&cache_dir(&dir), &fixture.noise_blob));
    assert!(object_absent(&cache_dir(&dir), &fixture.noise_commit));
}

#[test]
fn unrelated_branch_objects_are_not_fetched() {
    let fixture = fixture();
    let checkout = tempfile::tempdir().expect("checkout");
    let dir = checkout.path().join("build");
    let output = run_preflight(
        &fixture.remote,
        &fixture.new,
        &dir,
        &[&fixture.old],
        &[],
        None,
    );
    assert!(
        output.status.success(),
        "safe target failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cache = cache_dir(&dir);
    assert!(object_absent(&cache, &fixture.noise_blob));
    assert!(object_absent(&cache, &fixture.noise_commit));
    assert!(
        !object_absent(&cache, &fixture.new),
        "requested target must be present"
    );
    assert!(
        !object_absent(&cache, &fixture.old),
        "comparison ancestor must be present"
    );
}

#[test]
fn hydrated_downgrade_is_refused_without_unrelated_objects() {
    let fixture = fixture();
    let checkout = tempfile::tempdir().expect("checkout");
    let dir = checkout.path().join("build");
    let seeded = run_preflight(
        &fixture.remote,
        &fixture.old,
        &dir,
        &[&fixture.old],
        &[],
        None,
    );
    assert!(
        seeded.status.success(),
        "seed old commit: {}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    assert!(object_absent(&cache_dir(&dir), &fixture.new));
    let tools = tempfile::tempdir().expect("tools");
    let log = tools.path().join("network.log");
    let real = which_git();
    let wrapper = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexec {} \"$@\"\n",
        shell_quote(&log),
        shell_quote(&real),
    );
    let wrapper_path = tools.path().join("git");
    fs::write(&wrapper_path, wrapper).expect("logging wrapper");
    let _ = Command::new("chmod")
        .args(["0755", wrapper_path.to_str().expect("wrapper")])
        .status();
    let downgrade = run_preflight(
        &fixture.remote,
        &fixture.old,
        &dir,
        &[&fixture.new],
        &[],
        Some(tools.path()),
    );
    assert!(!downgrade.status.success());
    assert!(
        String::from_utf8_lossy(&downgrade.stderr).contains("Refusing Homeboy runner downgrade")
    );
    let network = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        network
            .lines()
            .any(|line| line.contains("fetch") && line.contains(&fixture.new)),
        "missing comparison commit must be hydrated: {network}"
    );
    assert!(
        !network.contains("refs/heads/*"),
        "hydration must not fetch every branch: {network}"
    );
    assert!(object_absent(&cache_dir(&dir), &fixture.noise_blob));
    assert!(object_absent(&cache_dir(&dir), &fixture.noise_commit));
}

#[test]
fn distinct_safe_target_reuses_object_storage() {
    let fixture = fixture();
    let checkout = tempfile::tempdir().expect("checkout");
    let dir = checkout.path().join("build");
    let first = run_preflight(
        &fixture.remote,
        &fixture.new,
        &dir,
        &[&fixture.old],
        &[],
        None,
    );
    assert!(
        first.status.success(),
        "distinct safe target failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let tools = tempfile::tempdir().expect("tools");
    let log = tools.path().join("network.log");
    write_git_wrapper(tools.path(), false, &log);
    let second = run_preflight(
        &fixture.remote,
        &fixture.new,
        &dir,
        &[&fixture.old],
        &[],
        Some(tools.path()),
    );
    assert!(
        second.status.success(),
        "reusable cache failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let network = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        network.is_empty(),
        "cached distinct target must not fetch again: {network}"
    );
    assert!(object_absent(&cache_dir(&dir), &fixture.noise_commit));
}

#[test]
fn stalled_fetch_fails_with_phase_timing() {
    let fixture = fixture();
    let checkout = tempfile::tempdir().expect("checkout");
    let dir = checkout.path().join("build");
    let tools = tempfile::tempdir().expect("tools");
    let log = tools.path().join("network.log");
    write_git_wrapper(tools.path(), true, &log);
    let script = materialize_script(
        fixture.remote.to_str().expect("source"),
        &fixture.new,
        dir.to_str().expect("dir"),
        dir.join("target/release/homeboy").to_str().expect("binary"),
        false,
        &[&fixture.old],
    );
    let guard = script
        .split_once("mkdir -p \"$(dirname \"$dir\")\"")
        .expect("preflight boundary")
        .0
        .to_string();
    let mut child = Command::new("bash");
    child
        .args(["-c", &guard])
        .env("HOMEBOY_REFRESH_FETCH_NO_PROGRESS_SECONDS", "1")
        .env("HOMEBOY_REFRESH_FETCH_DEADLINE_SECONDS", "4")
        .env(
            "PATH",
            format!(
                "{}:{}",
                tools.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child.spawn().expect("spawn stalled fetch");
    let started = Instant::now();
    let status = loop {
        if started.elapsed() > Duration::from_secs(8) {
            let _ = child.kill();
            panic!("stalled fetch was not bounded");
        }
        match child.try_wait().expect("poll fetch") {
            Some(status) => break status,
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    assert!(!status.success());
    assert!(
        stderr.contains("HOMEBOY_REFRESH_FETCH_NO_PROGRESS"),
        "missing no-progress evidence: {stderr}"
    );
    assert!(
        stderr.contains("HOMEBOY_REFRESH_FETCH_PHASE"),
        "missing phase timing: {stderr}"
    );
    assert!(stderr.contains("duration_ms="), "{stderr}");
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[test]
fn managed_checkout_identical_target_reuses_existing_objects_without_network() {
    let fixture = fixture();
    let checkout = tempfile::tempdir().expect("checkout");
    let dir = checkout.path().join("build");
    git(
        checkout.path(),
        &[
            "clone",
            "--quiet",
            "--no-local",
            "--single-branch",
            "--branch",
            "main",
            "--no-tags",
            fixture.remote.to_str().unwrap(),
            dir.to_str().unwrap(),
        ],
    );
    let tools = tempfile::tempdir().expect("tools");
    let log = tools.path().join("network.log");
    write_git_wrapper(tools.path(), false, &log);
    let output = run_preflight(
        &fixture.remote,
        &fixture.new,
        &dir,
        &[&fixture.new],
        &[],
        Some(tools.path()),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fs::read_to_string(log).unwrap_or_default().is_empty());
    assert!(object_absent(&cache_dir(&dir), &fixture.noise_blob));
}

#[test]
fn branch_precedence_and_cached_abbreviated_targets_are_preserved() {
    let fixture = fixture();
    git(&fixture.remote, &["tag", "collision", &fixture.old]);
    git(&fixture.remote, &["branch", "collision", &fixture.new]);
    let checkout = tempfile::tempdir().expect("checkout");
    let dir = checkout.path().join("build");
    let output = run_preflight(
        &fixture.remote,
        "collision",
        &dir,
        &[&fixture.old],
        &[],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!object_absent(&cache_dir(&dir), &fixture.new));
    let tools = tempfile::tempdir().expect("tools");
    let log = tools.path().join("network.log");
    write_git_wrapper(tools.path(), false, &log);
    let output = run_preflight(
        &fixture.remote,
        &fixture.new[..12],
        &dir,
        &[&fixture.new],
        &[],
        Some(tools.path()),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fs::read_to_string(log).unwrap_or_default().is_empty());
}

#[test]
fn shallow_identical_target_skips_network_and_distinct_authority_hydrates_history() {
    let fixture = fixture();
    let checkout = tempfile::tempdir().unwrap();
    let dir = checkout.path().join("build");
    let source = format!("file://{}", fixture.remote.display());
    git(
        checkout.path(),
        &[
            "clone",
            "--quiet",
            "--depth=1",
            "--branch=main",
            "--no-tags",
            &source,
            dir.to_str().unwrap(),
        ],
    );
    let tools = tempfile::tempdir().unwrap();
    let log = tools.path().join("network.log");
    write_git_wrapper(tools.path(), false, &log);
    let output = run_preflight(
        Path::new(&source),
        &fixture.new,
        &dir,
        &[&fixture.new],
        &[],
        Some(tools.path()),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fs::read_to_string(log).unwrap_or_default().is_empty());
    assert_eq!(
        git_output(&dir, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    let output = run_preflight(
        Path::new(&source),
        &fixture.new,
        &dir,
        &[&fixture.old],
        &[],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git_output(&dir, &["rev-parse", "--is-shallow-repository"]),
        "false"
    );
    assert!(!object_absent(&cache_dir(&dir), &fixture.old));
    assert!(object_absent(&cache_dir(&dir), &fixture.noise_commit));
}

#[test]
fn hard_deadline_kills_term_resistant_fetch_and_descendants() {
    let fixture = fixture();
    let checkout = tempfile::tempdir().expect("checkout");
    let dir = checkout.path().join("build");
    let tools = tempfile::tempdir().expect("tools");
    let wrapper = tools.path().join("git");
    fs::write(&wrapper, format!("#!/bin/bash\nfor arg in \"$@\"; do\n if [ \"$arg\" = fetch ]; then\n  trap '' TERM\n  (trap '' TERM; while :; do sleep 1; done) &\n  while :; do echo progress; sleep 0.1; done\n fi\ndone\nexec {} \"$@\"\n", shell_quote(&which_git()))).unwrap();
    assert!(Command::new("chmod")
        .args(["0755", wrapper.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let started = Instant::now();
    let output = run_preflight(
        &fixture.remote,
        &fixture.new,
        &dir,
        &[&fixture.old],
        &[
            ("HOMEBOY_REFRESH_FETCH_NO_PROGRESS_SECONDS", "5"),
            ("HOMEBOY_REFRESH_FETCH_DEADLINE_SECONDS", "1"),
        ],
        Some(tools.path()),
    );
    assert!(!output.status.success());
    assert!(started.elapsed() < Duration::from_secs(5));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("HOMEBOY_REFRESH_FETCH_DEADLINE"),
        "{stderr}"
    );
    assert!(
        stderr.contains("progress"),
        "network diagnostics retained: {stderr}"
    );
    assert!(stderr.contains("output_bytes="), "{stderr}");
}

#[test]
fn delayed_fetch_admits_local_cook_and_real_publication_is_fenced() {
    homeboy_core::test_support::with_isolated_home(|home| {
        assert!(
            homeboy_core::paths::runtime_promotion_dir()
                .unwrap()
                .starts_with(home.path()),
            "admission fixtures must never use the operator's promotion root"
        );
        let fixture = fixture();
        let checkout = tempfile::tempdir().unwrap();
        let dir = checkout.path().join("build");
        let tools = tempfile::tempdir().unwrap();
        let ready = tools.path().join("fetch-started");
        let release = tools.path().join("fetch-release");
        let wrapper = tools.path().join("git");
        fs::write(&wrapper, format!("#!/bin/sh\nfor arg in \"$@\"; do\n if [ \"$arg\" = fetch ]; then\n  touch {}\n  while [ ! -f {} ]; do sleep 0.05; done\n fi\ndone\nexec {} \"$@\"\n", shell_quote(&ready), shell_quote(&release), shell_quote(&which_git()))).unwrap();
        assert!(Command::new("chmod")
            .args(["0755", wrapper.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
        std::thread::scope(|scope| {
            let fetch = scope.spawn(|| {
                run_preflight(
                    &fixture.remote,
                    &fixture.new,
                    &dir,
                    &[&fixture.old],
                    &[("HOMEBOY_REFRESH_FETCH_NO_PROGRESS_SECONDS", "5")],
                    Some(tools.path()),
                )
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(ready.exists(), "real fetch reached its deliberate delay");
            let pin = homeboy_core::runtime_promotion::pin_cook_generation_waiting(
                "unrelated-local-cook",
                Duration::from_millis(200),
                || false,
                |_| {},
            )
            .expect("local Cook admitted while fetch is delayed");
            fs::write(&release, "release").unwrap();
            let output = fetch.join().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            drop(pin);
        });
        let roots = homeboy_core::paths::PathRoots::from_environment().unwrap();
        assert!(roots.data().starts_with(home.path()));
        crate::create_in_roots(
            &roots,
            r#"{"id":"publication-fixture","kind":"local","homeboy_path":"/old/homeboy"}"#,
            false,
        )
        .unwrap();
        let lease =
            acquire_runner_binary_promotion_in_roots(&roots, "publication-fixture", &fixture.new)
                .unwrap();
        let publication_pin = homeboy_core::runtime_promotion::pin_cook_generation_waiting(
            "publication-contender",
            Duration::from_millis(80),
            || false,
            |_| {},
        )
        .expect("runner-scoped publication admits unrelated local Cooks");
        let root = homeboy_core::paths::runtime_promotion_dir_in_root(roots.data());
        let contender = std::thread::spawn(move || {
            homeboy_core::runtime_promotion::acquire_in_root(
                &root,
                "competing selection",
                "publication-fixture",
            )
        })
        .join()
        .unwrap();
        assert!(contender.is_err(), "same-runner publication remains fenced");
        promote_verified_runner_binary_in_roots(
            &roots,
            &lease,
            "publication-fixture",
            "/verified/homeboy",
        )
        .unwrap();
        assert_eq!(
            crate::load_in_roots(&roots, "publication-fixture")
                .unwrap()
                .settings
                .homeboy_path
                .as_deref(),
            Some("/verified/homeboy")
        );
        drop(lease);
        drop(publication_pin);
        homeboy_core::runtime_promotion::pin_cook_generation("after-publication")
            .expect("admission restored");
    });
}
