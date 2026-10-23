//! Shared real-Git candidate construction for continuation fixtures.
use crate::agent_task::AgentTaskCandidate;
use std::path::Path;

pub(crate) fn snapshot(root: &Path, base: &str, run_id: &str) -> AgentTaskCandidate {
    let index = tempfile::NamedTempFile::new().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_INDEX_FILE", index.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["read-tree", base]);
    git(&["add", "--all"]);
    let tree = git(&["write-tree"]);
    let sha = git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit-tree",
        &tree,
        "-p",
        base,
        "-m",
        "candidate",
    ]);
    git(&[
        "update-ref",
        &format!("refs/homeboy/candidates/{run_id}"),
        &sha,
    ]);
    AgentTaskCandidate {
        base_sha: base.to_string(),
        candidate_sha: sha,
        tree_sha: tree,
    }
}
