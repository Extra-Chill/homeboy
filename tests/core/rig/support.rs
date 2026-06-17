use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test-only helper for the happy-path git flows shared across rig source and
/// install tests: repository init, identity-stamped commits, bare clones used
/// as remote sources, and pushes back to that source.
///
/// Tests that assert *failure* behavior or inspect raw git output should keep
/// using [`run_git`] / explicit `Command::new("git")` calls instead.
pub(crate) struct GitFixture<'a> {
    dir: &'a Path,
}

impl<'a> GitFixture<'a> {
    /// Initialize a git repository in `dir` on the `main` branch.
    pub(crate) fn init(dir: &'a Path) -> Self {
        run_git(dir, &["init", "-b", "main"]);
        Self { dir }
    }

    /// Attach to an already-initialized repository in `dir` (e.g. one created
    /// via a raw `git init` elsewhere) to reuse the commit/push helpers.
    ///
    /// `allow(dead_code)`: this shared support module is `#[path]`-included
    /// separately into each rig test file, so helpers used by only one of them
    /// (here, the source-update tests) appear unused from the other's view.
    #[allow(dead_code)]
    pub(crate) fn attach(dir: &'a Path) -> Self {
        Self { dir }
    }

    /// Stage everything and create a commit with a stable test identity.
    pub(crate) fn commit(&self, message: &str) {
        run_git(self.dir, &["add", "."]);
        run_git(
            self.dir,
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

    /// Clone the repository as a bare `rig-package.git` source inside a fresh
    /// temp dir, returning the parent [`tempfile::TempDir`] guard so the bare
    /// clone is cleaned up automatically. Use [`bare_source_path`] to derive
    /// the source string.
    pub(crate) fn clone_bare(&self) -> tempfile::TempDir {
        let bare = tempfile::tempdir().expect("bare parent");
        let source_path = bare.path().join("rig-package.git");
        run_git(
            self.dir,
            &[
                "clone",
                "--bare",
                self.dir.to_str().expect("package path utf8"),
                source_path.to_str().expect("bare path utf8"),
            ],
        );
        bare
    }

    /// Push the current `HEAD` to the `main` branch of `source`.
    ///
    /// `allow(dead_code)`: only the source-update tests push; see `attach`.
    #[allow(dead_code)]
    pub(crate) fn push_main(&self, source: &str) {
        run_git(self.dir, &["push", source, "HEAD:main"]);
    }
}

/// Path to the bare `rig-package.git` clone produced by
/// [`GitFixture::clone_bare`].
pub(crate) fn bare_source_path(bare: &tempfile::TempDir) -> PathBuf {
    bare.path().join("rig-package.git")
}
