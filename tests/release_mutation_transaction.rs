use homeboy_core::test_support::{HermeticTestContext, TestBinary};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ORIGINAL_VERSION: &str = "1.2.2";
const RELEASE_VERSION: &str = "1.2.3";
const TAG: &str = "v1.2.3";

#[test]
fn release_package_commit_and_tag_share_the_declared_version_identity() {
    let context = HermeticTestContext::new();
    let fixture = ReleaseFixture::new(&context, false);

    let output = fixture.release(&context);

    assert_success(&output);
    let release_head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    assert_ne!(release_head, fixture.original_head);
    assert_eq!(
        git(&fixture.repo, &["log", "-1", "--format=%s"]),
        "release: v1.2.3"
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", &format!("{TAG}^{{commit}}")]),
        release_head
    );
    assert_eq!(
        git(&fixture.remote, &["rev-parse", "refs/heads/main"]),
        release_head
    );
    assert_eq!(
        git(&fixture.remote, &["rev-parse", "refs/tags/v1.2.3^{commit}"]),
        release_head
    );

    let committed_target = git(&fixture.repo, &["show", "HEAD:fixture.php"]);
    assert_release_targets(&committed_target);
    assert!(git(&fixture.repo, &["show", "HEAD:CHANGELOG.md"]).contains(RELEASE_VERSION));

    let artifact = durable_artifact(&context);
    assert!(artifact
        .to_string_lossy()
        .contains("/release/fixture/1_2_3/01-fixture.zip"));
    assert_release_targets(&unzip_file(&artifact, "fixture.php"));
}

#[test]
fn package_that_discards_release_mutations_rolls_back_without_a_tag() {
    let context = HermeticTestContext::new();
    let fixture = ReleaseFixture::new(&context, true);

    let output = fixture.release(&context);

    assert!(
        !output.status.success(),
        "release unexpectedly succeeded: {}",
        output_text(&output)
    );
    let rendered = output_text(&output);
    assert!(
        rendered.contains("package dirtied tracked files"),
        "{rendered}"
    );
    assert!(
        rendered.contains("fixture.php") && rendered.contains("CHANGELOG.md"),
        "{rendered}"
    );
    assert!(
        rendered.contains("rollback") || rendered.contains("Rollback"),
        "{rendered}"
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "HEAD"]),
        fixture.original_head
    );
    assert_eq!(git(&fixture.repo, &["status", "--porcelain=v1"]), "");
    assert!(std::fs::read_to_string(fixture.repo.join("fixture.php"))
        .expect("restored version target")
        .contains(ORIGINAL_VERSION));
    assert!(std::fs::read_to_string(fixture.repo.join("CHANGELOG.md"))
        .expect("restored changelog")
        .contains("## Unreleased"));
    assert!(!ref_exists(&fixture.repo, &format!("refs/tags/{TAG}")));
    assert!(!ref_exists(&fixture.remote, &format!("refs/tags/{TAG}")));
    assert_eq!(
        git(&fixture.remote, &["rev-parse", "refs/heads/main"]),
        fixture.original_head
    );

    // The package was built from the bumped checkout before its action restored
    // tracked files. Its durable bytes remain diagnostic evidence, but cannot
    // authorize a commit, tag, or push after the ownership violation.
    assert_release_targets(&unzip_file(&durable_artifact(&context), "fixture.php"));
}

struct ReleaseFixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
    remote: PathBuf,
    original_head: String,
}

impl ReleaseFixture {
    fn new(context: &HermeticTestContext, discard_mutations: bool) -> Self {
        let root = tempfile::tempdir().expect("release fixture root");
        let repo = root.path().join("component");
        let remote = root.path().join("origin.git");
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
        .expect("version targets");
        std::fs::write(
            repo.join("CHANGELOG.md"),
            "# Changelog\n\n## Unreleased\n\n- Preserve generated release mutations\n",
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
        run(&repo, &["git", "commit", "-qm", "Initial fixture"]);

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

        install_package_extension(context, discard_mutations);
        let original_head = git(&repo, &["rev-parse", "HEAD"]);
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
                "--bump",
                "patch",
                "--skip-checks",
                "--apply",
                "--full",
            ])
            .output()
            .expect("run release fixture")
    }
}

fn install_package_extension(context: &HermeticTestContext, discard_mutations: bool) {
    let extension_dir = context.config_dir().join("extensions/fixture-packager");
    std::fs::create_dir_all(&extension_dir).expect("extension dir");
    let restore = if discard_mutations {
        "git restore fixture.php CHANGELOG.md;"
    } else {
        ""
    };
    let package_command = format!(
        "rm -rf build; mkdir -p build/stage; cp fixture.php CHANGELOG.md build/stage/; \
         (cd build/stage && zip -q ../fixture.zip fixture.php CHANGELOG.md); {restore} \
         printf '[{{\"path\":\"build/fixture.zip\",\"type\":\"archive\"}}]'"
    );
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

fn durable_artifact(context: &HermeticTestContext) -> PathBuf {
    let directory = context.artifact_dir().join("release/fixture/1_2_3");
    let mut artifacts = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| {
            panic!("read durable artifacts at {}: {error}", directory.display())
        })
        .map(|entry| entry.expect("artifact entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.starts_with("01-") && name.ends_with(".zip"))
        })
        .collect::<Vec<_>>();
    artifacts.sort();
    assert_eq!(
        artifacts.len(),
        1,
        "durable artifact inventory: {artifacts:?}"
    );
    artifacts.remove(0)
}

fn unzip_file(archive: &Path, file: &str) -> String {
    let output = Command::new("unzip")
        .args(["-p", archive.to_str().expect("archive path"), file])
        .output()
        .expect("read release archive");
    assert_success(&output);
    String::from_utf8(output.stdout).expect("archive text")
}

fn assert_release_targets(contents: &str) {
    assert!(
        contents.contains(&format!("Version: {RELEASE_VERSION}")),
        "{contents}"
    );
    assert!(
        contents.contains(&format!("FIXTURE_VERSION', '{RELEASE_VERSION}'")),
        "{contents}"
    );
    assert!(!contents.contains(ORIGINAL_VERSION), "{contents}");
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
