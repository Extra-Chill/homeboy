//! Git fixtures shared by tests; repository topology stays at the call site.

use std::path::Path;
use std::process::{Command, Output};

pub struct GitFixture<'a> {
    repo: &'a Path,
    identity: Option<(&'static str, &'static str)>,
}

impl<'a> GitFixture<'a> {
    /// Use the caller's environment and repository configuration unchanged.
    pub fn new(repo: &'a Path) -> Self {
        Self {
            repo,
            identity: None,
        }
    }

    pub fn with_identity(repo: &'a Path, name: &'static str, email: &'static str) -> Self {
        Self {
            repo,
            identity: Some((name, email)),
        }
    }

    pub fn execute(&self, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        command.args(args).current_dir(self.repo);
        if let Some((name, email)) = self.identity {
            command
                .env("GIT_AUTHOR_NAME", name)
                .env("GIT_AUTHOR_EMAIL", email)
                .env("GIT_COMMITTER_NAME", name)
                .env("GIT_COMMITTER_EMAIL", email);
        }
        command.output().expect("git fixture command")
    }
}

/// Initialize a main-branch repository with a deterministic local identity.
pub fn init(repo: impl AsRef<Path>) {
    let repo = repo.as_ref();
    std::fs::create_dir_all(repo).expect("git fixture directory");
    run(repo, &["init", "-q", "-b", "main"]);
    run(repo, &["config", "user.name", "Homeboy Test"]);
    run(repo, &["config", "user.email", "homeboy@example.com"]);
}

/// Create an empty temporary main-branch repository.
pub fn repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("git fixture tempdir");
    init(temp.path());
    temp
}

/// Run git without overriding caller configuration, panicking with failure evidence.
pub fn output(repo: impl AsRef<Path>, args: &[&str]) -> String {
    let output = GitFixture::new(repo.as_ref()).execute(args);
    assert!(
        output.status.success(),
        "git {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn run(repo: impl AsRef<Path>, args: &[&str]) {
    output(repo, args);
}

pub fn head(repo: impl AsRef<Path>) -> String {
    output(repo, &["rev-parse", "HEAD"])
}

pub fn commit_all(repo: impl AsRef<Path>, message: &str) {
    run(&repo, &["add", "."]);
    run(repo, &["commit", "-q", "-m", message]);
}

/// Write and stage only the named file, preserving other staged changes.
pub fn commit_file(repo: impl AsRef<Path>, name: &str, contents: &str, message: &str) {
    let repo = repo.as_ref();
    let file = repo.join(name);
    std::fs::create_dir_all(file.parent().expect("fixture parent")).expect("fixture directory");
    std::fs::write(file, contents).expect("fixture file");
    run(repo, &["add", name]);
    run(repo, &["commit", "-q", "-m", message]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_and_commits() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init(&repo);
        assert_eq!(output(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(output(&repo, &["config", "user.name"]), "Homeboy Test");
        assert_eq!(
            output(&repo, &["config", "user.email"]),
            "homeboy@example.com"
        );
        commit_file(&repo, "src/file", "one", "first");
        let first = head(&repo);
        std::fs::write(repo.join("src/file"), "two").unwrap();
        std::fs::write(repo.join("other"), "three").unwrap();
        commit_all(&repo, "second");
        assert_ne!(head(&repo), first);
        assert_eq!(output(&repo, &["status", "--porcelain"]), "");
        assert_eq!(output(&repo, &["show", "HEAD:other"]), "three");
    }

    #[test]
    #[should_panic(expected = "stderr=")]
    fn failures_include_stderr() {
        let temp = tempfile::tempdir().unwrap();
        head(temp.path());
    }
}
