use std::path::Path;
use std::process::Command;

#[test]
fn root_generated_artifacts_do_not_dirty_a_checkout_or_ignore_nested_build_source() {
    let fixture = tempfile::tempdir().expect("fixture");
    let repository = fixture.path();
    let source_build = repository.join("crates/example/build/source.rs");

    std::fs::create_dir_all(source_build.parent().expect("source build directory"))
        .expect("create nested source build directory");
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/.gitignore"),
        repository.join(".gitignore"),
    )
    .expect("copy root ignore policy");
    std::fs::write(&source_build, "pub const SOURCE: bool = true;\n").expect("write source");

    run_git(repository, &["init", "-q", "-b", "main"]);
    run_git(
        repository,
        &["add", ".gitignore", "crates/example/build/source.rs"],
    );
    run_git(
        repository,
        &[
            "-c",
            "user.name=Homeboy Test",
            "-c",
            "user.email=homeboy@example.test",
            "commit",
            "-q",
            "-m",
            "initial",
        ],
    );

    std::fs::create_dir_all(repository.join("build")).expect("release notes directory");
    std::fs::write(
        repository.join("build/v0.333.0-release-notes.md"),
        "generated release notes\n",
    )
    .expect("write generated release notes");
    std::fs::create_dir_all(repository.join(".cargo-target/debug"))
        .expect("cargo target directory");
    std::fs::write(
        repository.join(".cargo-target/debug/homeboy"),
        "cargo output\n",
    )
    .expect("write dedicated cargo output");

    assert_eq!(git_output(repository, &["status", "--porcelain"]), "");
    assert_eq!(
        git_output(
            repository,
            &[
                "ls-files",
                "--error-unmatch",
                "crates/example/build/source.rs"
            ],
        ),
        "crates/example/build/source.rs"
    );
}

fn run_git(repository: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(repository: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_string()
}
