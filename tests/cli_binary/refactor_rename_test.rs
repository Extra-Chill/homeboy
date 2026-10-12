use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn rename_defaults_to_cwd_git_worktree_without_component_metadata() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(root.path().join("src")).unwrap();
    fs::write(
        root.path().join("src/lib.rs"),
        "pub fn old_name() -> i32 { 1 }\npub fn call() -> i32 { old_name() }\n",
    )
    .unwrap();

    let git_init = Command::new("git")
        .arg("init")
        .current_dir(root.path())
        .output()
        .expect("git init");
    assert!(
        git_init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&git_init.stderr)
    );

    let output = homeboy_command()
        .args([
            "refactor", "rename", "--from", "old_name", "--to", "new_name", "--write",
        ])
        .current_dir(root.path())
        .env("HOME", root.path())
        .output()
        .expect("run homeboy refactor rename");

    assert!(
        output.status.success(),
        "refactor rename failed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let source = fs::read_to_string(root.path().join("src/lib.rs")).unwrap();
    assert!(source.contains("new_name"));
    assert!(!source.contains("old_name"));
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn homeboy_command() -> Command {
    let mut command = Command::new(homeboy_bin());
    command.env("HOMEBOY_NO_UPDATE_CHECK", "1");
    command
}
