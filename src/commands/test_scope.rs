use clap::Args;
use serde::Serialize;

use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct TestScopeArgs {
    /// Git ref to compare against when computing changed test scope.
    #[arg(long, value_name = "REF", default_value = "HEAD~10")]
    pub since: String,
}

#[derive(Serialize)]
#[allow(dead_code)]
pub struct TestScopeCommandOutput {
    pub status: String,
    pub changed_since: String,
}

#[allow(dead_code)]
pub fn run(args: TestScopeArgs, _global: &GlobalArgs) -> CmdResult<TestScopeCommandOutput> {
    Ok((
        TestScopeCommandOutput {
            status: "ready".to_string(),
            changed_since: args.since,
        },
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::code_audit::is_test_path;
    use homeboy::component::Component;
    use homeboy::extension::test::compute_changed_test_scope;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn test_run() {
        let (output, exit_code) = run(
            TestScopeArgs {
                since: "HEAD~5".to_string(),
            },
            &GlobalArgs {},
        )
        .expect("run should succeed");

        assert_eq!(exit_code, 0);
        assert_eq!(output.status, "ready");
        assert_eq!(output.changed_since, "HEAD~5");
    }

    #[test]
    fn test_is_test_path_uses_canonical() {
        // Verify canonical is_test_path (from code_audit::walker) is used
        assert!(is_test_path("tests/unit/foo_test.rs"));
        assert!(is_test_path("plugin/tests/FooTest.php"));
        assert!(is_test_path("src/components/Button.test.tsx"));
        assert!(!is_test_path("src/core/component.rs"));
    }

    #[test]
    fn test_compute_changed_test_scope() {
        let dir = TempDir::new().expect("temp dir should be created");
        let root = dir.path();

        fs::create_dir_all(root.join("src")).expect("src dir should be created");
        fs::create_dir_all(root.join("tests")).expect("tests dir should be created");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='scope-test'\nversion='0.1.0'\n",
        )
        .expect("Cargo.toml should be written");
        fs::write(root.join("src/lib.rs"), "pub fn thing() {}\n").expect("lib should be written");

        Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init should run");
        Command::new("git")
            .args(["config", "user.email", "tests@example.com"])
            .current_dir(root)
            .output()
            .expect("git config email should run");
        Command::new("git")
            .args(["config", "user.name", "Tests"])
            .current_dir(root)
            .output()
            .expect("git config name should run");
        Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .output()
            .expect("git add should run");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .expect("git commit should run");

        fs::write(root.join("tests/scope_test.rs"), "#[test]\nfn smoke(){}\n")
            .expect("test file should be written");
        Command::new("git")
            .args(["add", "tests/scope_test.rs"])
            .current_dir(root)
            .output()
            .expect("git add test file should run");
        Command::new("git")
            .args(["commit", "-m", "add test"])
            .current_dir(root)
            .output()
            .expect("git commit test file should run");

        let component = Component::new(
            "scope-test".to_string(),
            root.to_string_lossy().to_string(),
            "/tmp/remote".to_string(),
            None,
        );

        let output = compute_changed_test_scope(&component, "HEAD~1")
            .expect("scope computation should succeed");

        assert_eq!(output.mode, "changed");
        assert_eq!(output.changed_since, Some("HEAD~1".to_string()));
        assert!(
            output
                .selected_files
                .iter()
                .any(|f| f.ends_with("tests/scope_test.rs")),
            "expected changed test file to be included"
        );
    }
}
