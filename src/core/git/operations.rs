mod build;
mod bulk;
mod changes_project;
mod commit;
mod constants;
mod fetch_and;
mod git_output;
mod helpers;
mod pull;
mod push;
mod status;
mod tag;
mod tag_exists;
mod types;

pub use build::*;
pub use bulk::*;
pub use changes_project::*;
pub use commit::*;
pub use constants::*;
pub use fetch_and::*;
pub use git_output::*;
pub use helpers::*;
pub use pull::*;
pub use push::*;
pub use status::*;
pub use tag::*;
pub use tag_exists::*;
pub use types::*;

use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::component;
use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::project;
use crate::release::changelog;

use super::changes::*;
use super::commits::*;
use super::primitives::is_git_repo;
use super::{execute_git, resolve_target};

    "node_modules",
    "dist",
    "build",
    "coverage",
    ".next",
    "vendor",
    "target",
    ".cache",
];

impl GitOutput {
    fn from_output(id: String, path: String, action: &str, output: std::process::Output) -> Self {
        Self {
            component_id: id,
            path,
            action: action.to_string(),
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }
}

// === Changes Output Types ===

// Input types for JSON parsing
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_workdir_clean_returns_true_for_clean_repo() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let path = temp_dir.path();

        // Initialize a git repo
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("Failed to init git repo");

        // Configure git user for commits
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("Failed to configure git email");

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .expect("Failed to configure git name");

        // Create a file and commit it
        fs::write(path.join("test.txt"), "content").expect("Failed to write file");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("Failed to git add");

        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()
            .expect("Failed to commit");

        // Now the repo should be clean
        assert!(
            super::super::is_workdir_clean(path),
            "Expected clean repo to return true"
        );
    }

    #[test]
    fn is_workdir_clean_returns_false_for_dirty_repo() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let path = temp_dir.path();

        // Initialize a git repo
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("Failed to init git repo");

        // Create an untracked file
        fs::write(path.join("untracked.txt"), "content").expect("Failed to write file");

        // Repo should be dirty (untracked file)
        assert!(
            !super::super::is_workdir_clean(path),
            "Expected dirty repo to return false"
        );
    }

    #[test]
    fn is_workdir_clean_returns_false_for_invalid_path() {
        let path = std::path::Path::new("/nonexistent/path/that/does/not/exist");
        assert!(
            !super::super::is_workdir_clean(path),
            "Expected invalid path to return false"
        );
    }
}
