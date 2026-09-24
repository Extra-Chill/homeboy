//! Regression coverage for #14974.
//!
//! `preflight.remote_sync` can fast-forward the release checkout's HEAD past
//! the commit range that produced the auto-detected bump type, because that
//! bump type is resolved BEFORE remote_sync runs. When a `feat:` commit lands
//! on the remote after auto-detection saw only `fix:` commits, the rebuilt
//! plan's `preflight.bump_policy` step recomputes "recommended" as `minor`
//! from the now-advanced HEAD while "requested" still reflects the older,
//! `fix:`-only range — an underbump false positive that fails the release
//! outright (`Requested patch bump is lower than detected minor impact`).
//!
//! This reproduces the exact shape of the production failure: a checkout
//! whose local HEAD sits one `fix:` commit ahead of the last release tag,
//! while `origin/main` has already advanced past it with a `feat:` commit
//! pushed by someone else. A release run with no explicit `--bump` must
//! auto-detect `minor` end to end — not fail asking for a lower bump than it
//! itself detects.

use homeboy_core::test_support::{HermeticTestContext, TestBinary};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ORIGINAL_VERSION: &str = "1.0.0";
const RELEASED_VERSION: &str = "1.1.0";
const TAG: &str = "v1.1.0";

#[test]
fn auto_detected_bump_survives_a_remote_sync_fast_forward_past_a_feat_commit() {
    let context = HermeticTestContext::new();
    let fixture = BumpPolicyFixture::new(&context);

    let output = fixture.release(&context);

    assert!(
        output.status.success(),
        "release should auto-detect minor end to end, not fail as an underbump: {}",
        output_text(&output)
    );

    // Assert directly on the release command's own reported bump type, not
    // just its side effects. This is the exact field CI reads for
    // `release-bump-type`: it must reflect the fresh, post-remote-sync
    // detection ("minor"), not the stale pre-fetch guess ("patch") that
    // caused #14974's false-positive underbump failure.
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("release command must emit its JSON envelope");
    assert_eq!(
        envelope["data"]["result"]["bump_type"], "minor",
        "reported bump_type must track the post-remote-sync detection: {envelope:#}"
    );
    assert_eq!(envelope["data"]["result"]["status"], "released");
    assert_eq!(envelope["data"]["result"]["new_version"], RELEASED_VERSION);
    assert_eq!(envelope["data"]["result"]["tag"], TAG);

    // The release commit must be the MINOR version the feat commit demands,
    // proving "requested" tracked "recommended" after remote_sync moved HEAD
    // — not the stale patch bump auto-detection saw before the fast-forward.
    assert_eq!(
        git(&fixture.repo, &["log", "-1", "--format=%s"]),
        format!("release: {TAG}")
    );
    assert!(ref_exists(&fixture.repo, &format!("refs/tags/{TAG}")));
    assert!(ref_exists(&fixture.remote, &format!("refs/tags/{TAG}")));

    let released = std::fs::read_to_string(fixture.repo.join("fixture.php"))
        .expect("read released version target");
    assert!(
        released.contains(&format!("Version: {RELEASED_VERSION}")),
        "{released}"
    );
    assert!(!released.contains(ORIGINAL_VERSION), "{released}");

    // remote_sync must have actually fast-forwarded past the feat commit that
    // landed on origin after the checkout's HEAD was pinned — otherwise this
    // test would not be exercising the bug at all.
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "log",
                "--format=%s",
                &format!("{}..HEAD", fixture.original_head)
            ]
        )
        .lines()
        .filter(|line| !line.is_empty())
        .any(|line| line == "feat: add remote capability"),
        true,
        "release history must include the feat commit pushed to origin after checkout"
    );
}

struct BumpPolicyFixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
    remote: PathBuf,
    original_head: String,
}

impl BumpPolicyFixture {
    fn new(context: &HermeticTestContext) -> Self {
        let root = tempfile::tempdir().expect("bump policy fixture root");
        let repo = root.path().join("component");
        let remote = root.path().join("origin.git");
        let bystander = root.path().join("bystander");
        std::fs::create_dir(&repo).expect("component repo");
        run(&repo, &["git", "init", "-q", "--initial-branch", "main"]);
        run(&repo, &["git", "config", "user.name", "Homeboy Fixture"]);
        run(
            &repo,
            &["git", "config", "user.email", "fixture@example.com"],
        );

        std::fs::write(
            repo.join("fixture.php"),
            format!(
                "<?php\n/* Version: {ORIGINAL_VERSION} */\ndefine('FIXTURE_VERSION', '{ORIGINAL_VERSION}');\n"
            ),
        )
        .expect("version target");
        std::fs::write(
            repo.join("CHANGELOG.md"),
            "# Changelog\n\n## Unreleased\n\n- Seed changelog\n",
        )
        .expect("changelog");
        std::fs::write(
            repo.join("homeboy.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "id": "fixture",
                "extensions": { "fixture-packager": {} },
                "version_targets": [
                    {
                        "file": "fixture.php",
                        "pattern": "Version:\\s*([0-9.]+)"
                    },
                    {
                        "file": "fixture.php",
                        "pattern": "FIXTURE_VERSION',\\s*'([0-9.]+)'"
                    }
                ],
                "changelog_target": "CHANGELOG.md"
            }))
            .expect("portable component config"),
        )
        .expect("homeboy config");
        run(&repo, &["git", "add", "."]);
        run(&repo, &["git", "commit", "-qm", "chore: initial fixture"]);

        run(
            root.path(),
            &["git", "init", "-q", "--bare", remote.to_str().unwrap()],
        );
        run(
            &repo,
            &["git", "remote", "add", "origin", remote.to_str().unwrap()],
        );
        run(&repo, &["git", "push", "-q", "-u", "origin", "main"]);
        run(&remote, &["git", "symbolic-ref", "HEAD", "refs/heads/main"]);
        run(&repo, &["git", "tag", &format!("v{ORIGINAL_VERSION}")]);
        run(
            &repo,
            &[
                "git",
                "push",
                "-q",
                "origin",
                &format!("v{ORIGINAL_VERSION}"),
            ],
        );

        // The checkout under test advances by one `fix:` commit and pushes it.
        // Auto-detection resolved against exactly this HEAD sees a fix-only
        // range and requests `patch`.
        std::fs::write(
            repo.join("fixture.php"),
            format!(
                "<?php\n/* Version: {ORIGINAL_VERSION} */\ndefine('FIXTURE_VERSION', '{ORIGINAL_VERSION}');\n// patched\n"
            ),
        )
        .expect("patch edit");
        run(&repo, &["git", "add", "."]);
        run(&repo, &["git", "commit", "-qm", "fix: patch something"]);
        run(&repo, &["git", "push", "-q", "origin", "main"]);
        let original_head = git(&repo, &["rev-parse", "HEAD"]);

        // A second contributor clones the SAME origin and pushes a `feat:`
        // commit on top — exactly the "someone else pushed a feat commit
        // after this checkout's HEAD was pinned" shape from #14974. The
        // release checkout (`repo`) never pulls this directly; only
        // `preflight.remote_sync`, during the release run, may fast-forward
        // it to include this commit.
        run(
            root.path(),
            &[
                "git",
                "clone",
                "-q",
                remote.to_str().unwrap(),
                bystander.to_str().unwrap(),
            ],
        );
        run(&bystander, &["git", "config", "user.name", "Bystander"]);
        run(
            &bystander,
            &["git", "config", "user.email", "bystander@example.com"],
        );
        std::fs::write(
            bystander.join("fixture.php"),
            format!(
                "<?php\n/* Version: {ORIGINAL_VERSION} */\ndefine('FIXTURE_VERSION', '{ORIGINAL_VERSION}');\n// patched\n// featured\n"
            ),
        )
        .expect("feat edit");
        run(&bystander, &["git", "add", "."]);
        run(
            &bystander,
            &["git", "commit", "-qm", "feat: add remote capability"],
        );
        run(&bystander, &["git", "push", "-q", "origin", "main"]);

        install_package_extension(context);
        Self {
            _root: root,
            repo,
            remote,
            original_head,
        }
    }

    fn release(&self, context: &HermeticTestContext) -> Output {
        context
            .command(TestBinary::HomeboyFixture)
            .args([
                "release",
                "fixture",
                "--path",
                self.repo.to_str().expect("repo path"),
                "--skip-checks",
                "--apply",
                "--full",
            ])
            .output()
            .expect("run release fixture")
    }
}

fn install_package_extension(context: &HermeticTestContext) {
    let extension_dir = context.config_dir().join("extensions/fixture-packager");
    std::fs::create_dir_all(&extension_dir).expect("extension dir");
    let package_command = "rm -rf build; mkdir -p build/stage; \
         cp fixture.php CHANGELOG.md build/stage/; \
         (cd build/stage && zip -q ../fixture.zip fixture.php CHANGELOG.md); \
         printf '[{\"path\":\"build/fixture.zip\",\"type\":\"archive\"}]'"
        .to_string();
    let manifest = serde_json::json!({
        "name": "Fixture Packager",
        "version": "1.0.0",
        "actions": [
            {
                "id": "release.package",
                "label": "Package release",
                "type": "command",
                "command": package_command
            },
            {
                "id": "release.publish",
                "label": "Publish release",
                "type": "command",
                "command": "true"
            }
        ]
    });
    std::fs::write(
        extension_dir.join("fixture-packager.json"),
        serde_json::to_vec_pretty(&manifest).expect("extension manifest"),
    )
    .expect("write extension manifest");
}

fn ref_exists(repo: &Path, reference: &str) -> bool {
    Command::new("git")
        .args(["show-ref", "--verify", "--quiet", reference])
        .current_dir(repo)
        .status()
        .expect("inspect git ref")
        .success()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let mut command = vec!["git"];
    command.extend_from_slice(args);
    let output = run_output(repo, &command);
    assert_success(&output);
    String::from_utf8(output.stdout)
        .expect("git output")
        .trim()
        .to_string()
}

fn run(repo: &Path, command: &[&str]) {
    let output = run_output(repo, command);
    assert_success(&output);
}

fn run_output(repo: &Path, command: &[&str]) -> Output {
    Command::new(command[0])
        .args(&command[1..])
        .current_dir(repo)
        .output()
        .unwrap_or_else(|error| panic!("run {command:?}: {error}"))
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}",
        output_text(output)
    );
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
