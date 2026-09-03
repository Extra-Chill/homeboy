use std::fs;
use std::path::{Path, PathBuf};

pub(crate) use homeboy_core::test_support::GitFixture;

pub(crate) fn write_rig(package: &Path, id: &str, body: &str) -> PathBuf {
    let rig_dir = package.join("rigs").join(id);
    fs::create_dir_all(&rig_dir).expect("rig dir");
    let rig_path = rig_dir.join("rig.json");
    fs::write(&rig_path, body).expect("rig json");
    rig_path
}

pub(crate) fn minimal_rig(id: &str) -> String {
    format!(
        r#"{{
            "id": "{}",
            "description": "{} rig",
            "components": {{
                "app": {{ "path": "${{env.DEV_ROOT}}/{}" }}
            }},
            "pipeline": {{
                "check": [{{ "kind": "check", "label": "app exists", "file": "${{components.app.path}}" }}]
            }}
        }}"#,
        id, id, id
    )
}

pub(crate) fn minimal_stack(id: &str, component: &str) -> String {
    format!(
        r#"{{
            "id": "{}",
            "description": "{} stack",
            "component": "{}",
            "component_path": "${{env.DEV_ROOT}}/{}",
            "base": {{ "remote": "origin", "branch": "main" }},
            "target": {{ "remote": "origin", "branch": "dev/combined-fixes" }},
            "prs": []
        }}"#,
        id, id, component, component
    )
}

pub(crate) fn write_stack(package: &Path, id: &str, component: &str) -> PathBuf {
    let stacks_dir = package.join("stacks");
    fs::create_dir_all(&stacks_dir).expect("stacks dir");
    let stack_path = stacks_dir.join(format!("{}.json", id));
    fs::write(&stack_path, minimal_stack(id, component)).expect("stack json");
    stack_path
}

pub(crate) fn run_git(dir: &Path, args: &[&str]) {
    let output = GitFixture::new(dir).execute(args);
    assert!(
        output.status.success(),
        "git {:?} failed: {}{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) fn init_main(dir: &Path) {
    run_git(dir, &["init", "-b", "main"]);
}

pub(crate) fn commit_all(dir: &Path, message: &str) {
    run_git(dir, &["add", "."]);
    run_git(
        dir,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            message,
        ],
    );
}

pub(crate) fn clone_bare(dir: &Path) -> tempfile::TempDir {
    let bare = tempfile::tempdir().expect("bare parent");
    run_git(
        dir,
        &[
            "clone",
            "--bare",
            dir.to_str().expect("package path utf8"),
            bare_source_path(&bare).to_str().expect("bare path utf8"),
        ],
    );
    bare
}

pub(crate) fn push_main(dir: &Path, source: &str) {
    run_git(dir, &["push", source, "HEAD:main"]);
}

#[allow(dead_code)]
pub(crate) fn bare_source_path(bare: &tempfile::TempDir) -> PathBuf {
    bare.path().join("rig-package.git")
}
