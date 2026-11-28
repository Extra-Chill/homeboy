//! A default-branch checkout with unreleased work must not plan or apply
//! from stale tracking refs. After refresh, preview and apply use the same SHA.

use homeboy_core::test_support::{HermeticTestContext, TestBinary};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ORIGINAL_VERSION: &str = "1.0.0";
const RELEASED_VERSION: &str = "1.1.0";
const TAG: &str = "v1.1.0";

#[test]
fn stale_release_requires_refresh_and_a_reviewed_source() {
    let context = HermeticTestContext::new();
    let fixture = BumpPolicyFixture::new(&context);
    let upstream = git(&fixture.remote, &["rev-parse", "main"]);
    assert_ne!(upstream, fixture.original_head);
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "origin/main"]),
        fixture.original_head
    );

    for args in [["--dry-run"], ["--apply"]] {
        let blocked = fixture.release(&context, &args);
        assert!(!blocked.status.success());
        let text = output_text(&blocked);
        assert!(text.contains("0 ahead, 1 behind"), "{text}");
        assert!(text.contains(&upstream), "{text}");
        assert!(text.contains(&fixture.original_head), "{text}");
        assert!(text.contains("fetched at"), "{text}");
        assert_eq!(
            git(&fixture.repo, &["rev-parse", "HEAD"]),
            fixture.original_head
        );
        assert!(!ref_exists(&fixture.remote, &format!("refs/tags/{TAG}")));
    }

    run(&fixture.repo, &["git", "merge", "--ff-only", "origin/main"]);
    let rejected = fixture.release(
        &context,
        &["--apply", "--expect-source", &fixture.original_head],
    );
    assert!(!rejected.status.success());
    assert!(output_text(&rejected).contains("Release source drift"));

    let preview = fixture.release(&context, &["--dry-run"]);
    assert!(preview.status.success(), "{}", output_text(&preview));
    let preview: serde_json::Value = serde_json::from_slice(&preview.stdout).expect("preview JSON");
    let planned = &preview["data"]["result"];
    assert_eq!(planned["status"], "planned");
    assert_eq!(planned["releasable_commits"], 2);
    assert_eq!(planned["new_version"], RELEASED_VERSION);
    assert_eq!(planned["plan"]["policy"]["source_sha"], upstream);
    let freshness = &planned["plan"]["policy"]["source_freshness"];
    assert_eq!(freshness["local_head"], upstream);
    assert_eq!(freshness["upstream_head"], upstream);
    assert_eq!(freshness["ahead"], 0);
    assert_eq!(freshness["behind"], 0);
    assert!(freshness["fetched_at"].as_str().is_some());

    let output = fixture.release(&context, &["--apply", "--expect-source", &upstream]);

    assert!(
        output.status.success(),
        "release should auto-detect minor end to end, not fail as an underbump: {}",
        output_text(&output)
    );

    // CI reads this field, so it must reflect the reviewed feature commit.
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("release command must emit its JSON envelope");
    assert_eq!(
        envelope["data"]["result"]["bump_type"], "minor",
        "reported bump_type must track the reviewed source: {envelope:#}"
    );
    assert_eq!(envelope["data"]["result"]["status"], "released");
    assert_eq!(envelope["data"]["result"]["new_version"], RELEASED_VERSION);
    assert_eq!(envelope["data"]["result"]["tag"], TAG);

    assert_eq!(
        envelope["data"]["result"]["plan"]["policy"]["source_sha"],
        upstream
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD^"]), upstream);
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
        run(&repo, &["git", "branch", "--unset-upstream"]);

        // A second contributor clones the SAME origin and pushes a `feat:`
        // commit on top — exactly the "someone else pushed a feat commit
        // after this checkout's HEAD was pinned" shape from #14974. The
        // release checkout (`repo`) does not fetch it before the test starts.
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

    fn release(&self, context: &HermeticTestContext, args: &[&str]) -> Output {
        context
            .command(TestBinary::HomeboyFixture)
            .args([
                "release",
                "fixture",
                "--path",
                self.repo.to_str().expect("repo path"),
                "--skip-checks",
                "--full",
            ])
            .args(args)
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

use homeboy_test_support::git::output as git;

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
