//! End-to-end repository-cleanliness gate (#16146, epic #16143).
//!
//! Homeboy adds exactly one file to a repository: `homeboy.json`. Every other
//! piece of state, evidence, scratch, and receipt lives outside the checkout.
//! This gate runs the real binary's repo-touching verbs against a fixture
//! checkout that contains only `homeboy.json` and one source file, then fails
//! if `git status --porcelain --ignored --untracked-files=all` reports any
//! path the verb was not supposed to produce. Each failure names the offending
//! path and the exact command that wrote it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Output;

use homeboy_core::test_support::{bounded_output, HermeticTestContext, TestBinary};

const COMPONENT_ID: &str = "fixture";
// Do not advertise lint: script-only lint is not supported (#16196). Cook
// automatically adds a `homeboy review lint` gate when lint is declared, which
// resolves an ambient Homeboy on PATH rather than this test's fixture binary.
// Keep the explicit `--verify true` gate below; this fixture guards checkout
// cleanliness, not extension-backed lint capability.
const HOMEBOY_JSON: &str = r#"{"id":"fixture","scripts":{"test":["true"],"build":["true"]}}"#;
/// The single source file. Its content matches the deterministic fixture
/// provider's default patch (`docs/agent-task-smoke.md`: `before` -> `after`),
/// so a Cook run has exactly one intended edit.
const SOURCE_FILE: &str = "docs/agent-task-smoke.md";
const SOURCE_BEFORE: &str = "before\n";
const SOURCE_AFTER: &str = "after\n";

/// One repo-touching verb and the exit code it must produce. Requiring the
/// exit code keeps a verb that dies early from passing the gate vacuously.
struct Verb {
    args: Vec<&'static str>,
    expected_exit: i32,
}

/// Verbs run with `--placement local --wait --path <repo>` appended.
///
/// `review lint` and the `review` umbrella are deliberately absent: they fail
/// with "No extension provider" for a script-only component, a separate bug
/// (#16196). Add them here once that is fixed.
fn review_verbs() -> Vec<Verb> {
    vec![
        Verb {
            args: vec!["review", "test", COMPONENT_ID],
            expected_exit: 0,
        },
        Verb {
            args: vec!["review", "build", COMPONENT_ID],
            expected_exit: 0,
        },
    ]
}

#[test]
fn repository_cleanliness_review_verbs_leave_checkout_pristine() {
    let context = HermeticTestContext::new();
    let fixture = Fixture::new();
    let mut failures = Vec::new();

    for verb in review_verbs() {
        fixture.make_pristine();
        let mut args: Vec<OsString> = verb.args.iter().map(OsString::from).collect();
        args.extend(["--placement", "local", "--wait", "--path"].map(OsString::from));
        args.push(fixture.repo.clone().into_os_string());

        let rendered = render_command(&args);
        let output = run_homeboy(&context, &args);
        if output.status.code() != Some(verb.expected_exit) {
            failures.push(format!(
                "`{rendered}` exited {:?}, expected {}\nstdout: {}\nstderr: {}",
                output.status.code(),
                verb.expected_exit,
                tail(&output.stdout),
                tail(&output.stderr),
            ));
        }
        failures.extend(offending_paths(&fixture.repo, &[], &rendered));
    }

    assert!(
        failures.is_empty(),
        "Homeboy must add nothing but homeboy.json to a repository:\n\n{}",
        failures.join("\n\n")
    );
}

#[test]
fn repository_cleanliness_fixture_cook_leaves_only_the_provider_edit() {
    let context = HermeticTestContext::new();
    let fixture = Fixture::new();
    // Cook's base preflight resolves `main` through `origin`; point it at the
    // fixture itself so the run stays offline.
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            fixture.repo.to_str().expect("fixture repository path"),
        ],
    );
    fixture.make_pristine();
    std::fs::write(
        context.config_dir().join("homeboy.json"),
        r#"{"retention":{"reconstructable_artifact_reserve_bytes":0}}"#,
    )
    .expect("disable host-capacity admission for the fixture checkout");

    let register: Vec<OsString> = vec![
        "component".into(),
        "create".into(),
        "--local-path".into(),
        fixture.repo.clone().into_os_string(),
    ];
    let registered = run_homeboy(&context, &register);
    assert!(
        registered.status.success(),
        "register fixture component: stdout={} stderr={}",
        tail(&registered.stdout),
        tail(&registered.stderr)
    );
    let mut failures = offending_paths(&fixture.repo, &[], &render_command(&register));

    // The Cook task checkout is a sibling worktree, outside the source checkout.
    let task_worktree = fixture.root.path().join("fixture-cook-task");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "repository-cleanliness-cook",
            task_worktree.to_str().expect("task worktree path"),
        ],
    );

    let cook: Vec<OsString> = [
        "--placement",
        "local",
        "--wait",
        "agent-task",
        "cook",
        "--run-id",
        "repository-cleanliness-cook",
        "--repo",
        COMPONENT_ID,
        "--backend",
        "fixture",
        "--model",
        "fixture-model",
        "--prompt",
        "apply the deterministic fixture edit",
        "--cwd",
        task_worktree.to_str().expect("task worktree path"),
        "--to-worktree",
        task_worktree.to_str().expect("task worktree path"),
        "--verify",
        "true",
        "--max-attempts",
        "1",
        "--no-finalize",
    ]
    .map(OsString::from)
    .to_vec();
    let rendered = render_command(&cook);
    // Reproduce the Release wrapper's ambient Homeboy on PATH. A cleanliness
    // fixture must not acquire an undeclared nested lint gate from that binary.
    let ambient_bin = context.root().join("ambient-bin");
    std::fs::create_dir(&ambient_bin).expect("ambient bin directory");
    homeboy_test_support::write_executable(
        &ambient_bin.join("homeboy"),
        "#!/bin/sh\nprintf 'unexpected ambient Homeboy invocation\\n' >&2\nexit 93\n",
        0o755,
    )
    .expect("poison ambient Homeboy");
    let path = std::env::join_paths(std::iter::once(ambient_bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").expect("test PATH"),
    )))
    .expect("fixture PATH");
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command.env("PATH", path);
    // Pin a quiet load reading so local admission does not depend on how busy
    // the host is (see cook_lab_handoff_test, homeboy#14984).
    command
        .env("GITHUB_ACTIONS", "true")
        .env("HOMEBOY_TEST_LOAD_AVERAGES", "0,0,0")
        .args(&cook);
    let output = bounded_output(command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "`{rendered}` must succeed\nstdout: {}\nstderr: {}",
        tail(&output.stdout),
        tail(&output.stderr)
    );

    // The source checkout is untouched; the task checkout holds exactly the
    // provider's intended edit and nothing else. Promotion materializes the
    // candidate commit's tree into the index and worktree (#16303), so the
    // edit is staged rather than a loose worktree patch.
    failures.extend(offending_paths(&fixture.repo, &[], &rendered));
    failures.extend(offending_paths(
        &task_worktree,
        &[format!("M  {SOURCE_FILE}")],
        &rendered,
    ));
    let edited = std::fs::read_to_string(task_worktree.join(SOURCE_FILE)).unwrap_or_default();
    if edited != SOURCE_AFTER {
        failures.push(format!(
            "`{rendered}` did not apply the provider edit to {SOURCE_FILE} in {}: {edited:?}",
            task_worktree.display()
        ));
    }

    assert!(
        failures.is_empty(),
        "Homeboy must add nothing but homeboy.json and the provider's edit:\n\n{}",
        failures.join("\n\n")
    );
}

/// A committed fixture repository (branch `main`) with only `homeboy.json` and
/// one source file, held in a temp root separate from the isolated HOME.
struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture root");
        let repo = root.path().join("repository");
        std::fs::create_dir_all(repo.join("docs")).expect("fixture repository");
        git(&repo, &["init", "-q", "--initial-branch", "main"]);
        git(&repo, &["config", "user.email", "homeboy@example.test"]);
        git(&repo, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(repo.join(SOURCE_FILE), SOURCE_BEFORE).expect("source file");
        std::fs::write(repo.join("homeboy.json"), HOMEBOY_JSON).expect("homeboy.json");
        git(&repo, &["add", SOURCE_FILE, "homeboy.json"]);
        git(&repo, &["commit", "-q", "-m", "fixture"]);
        Self { root, repo }
    }

    fn make_pristine(&self) {
        git(&self.repo, &["reset", "-q", "--hard"]);
        git(&self.repo, &["clean", "-q", "-ffdx"]);
        assert!(
            porcelain(&self.repo).is_empty(),
            "fixture checkout is not pristine before the verb runs"
        );
    }
}

/// Report each `git status` entry in `checkout` that is not `allowed`.
fn offending_paths(checkout: &Path, allowed: &[String], command: &str) -> Vec<String> {
    porcelain(checkout)
        .into_iter()
        .filter(|entry| !allowed.contains(entry))
        .map(|entry| {
            format!(
                "{} in {} written by `{command}`",
                entry.trim(),
                checkout.display()
            )
        })
        .collect()
}

fn porcelain(checkout: &Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--ignored",
            "--untracked-files=all",
        ])
        .current_dir(checkout)
        .output()
        .expect("git status");
    assert!(
        output.status.success(),
        "git status failed in {}: {}",
        checkout.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn run_homeboy(context: &HermeticTestContext, args: &[OsString]) -> Output {
    let mut command = context.command(TestBinary::HomeboyFixture);
    command.args(args);
    bounded_output(command)
}

fn render_command(args: &[OsString]) -> String {
    std::iter::once("homeboy".to_string())
        .chain(args.iter().map(|arg| arg.to_string_lossy().into_owned()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(4000)..].iter().collect()
}

fn git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}
