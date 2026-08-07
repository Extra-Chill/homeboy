use std::path::{Path, PathBuf};

pub(crate) fn git_watch_paths(
    git_dir: &Path,
    common_dir: &Path,
    head: Option<&str>,
    snapshot_note_ref: &str,
) -> Vec<PathBuf> {
    let mut paths = vec![
        git_dir.join("HEAD"),
        common_dir.join("packed-refs"),
        common_dir.join(snapshot_note_ref),
    ];
    if let Some(reference) = head.and_then(|head| head.trim().strip_prefix("ref: ")) {
        let ref_dir =
            if reference.starts_with("refs/bisect/") || reference.starts_with("refs/worktree/") {
                git_dir
            } else {
                common_dir
            };
        paths.push(ref_dir.join(reference));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watches_symbolic_ref_in_common_git_directory_for_linked_worktree() {
        let paths = git_watch_paths(
            Path::new("/repo/.git/worktrees/feature"),
            Path::new("/repo/.git"),
            Some("ref: refs/heads/feature\n"),
            "refs/notes/homeboy-snapshot",
        );

        assert_eq!(
            paths,
            vec![
                PathBuf::from("/repo/.git/worktrees/feature/HEAD"),
                PathBuf::from("/repo/.git/packed-refs"),
                PathBuf::from("/repo/.git/refs/notes/homeboy-snapshot"),
                PathBuf::from("/repo/.git/refs/heads/feature"),
            ]
        );
    }

    #[test]
    fn watches_primary_worktree_ref_and_packed_refs() {
        let paths = git_watch_paths(
            Path::new("/repo/.git"),
            Path::new("/repo/.git"),
            Some("ref: refs/heads/main\n"),
            "refs/notes/homeboy-snapshot",
        );

        assert_eq!(
            paths,
            vec![
                PathBuf::from("/repo/.git/HEAD"),
                PathBuf::from("/repo/.git/packed-refs"),
                PathBuf::from("/repo/.git/refs/notes/homeboy-snapshot"),
                PathBuf::from("/repo/.git/refs/heads/main"),
            ]
        );
    }

    #[test]
    fn detached_head_watches_git_metadata_without_a_symbolic_ref() {
        let paths = git_watch_paths(
            Path::new("/repo/.git/worktrees/detached"),
            Path::new("/repo/.git"),
            Some("9e102eb708131db18e55e100cc47262694148056\n"),
            "refs/notes/homeboy-snapshot",
        );

        assert_eq!(
            paths,
            vec![
                PathBuf::from("/repo/.git/worktrees/detached/HEAD"),
                PathBuf::from("/repo/.git/packed-refs"),
                PathBuf::from("/repo/.git/refs/notes/homeboy-snapshot"),
            ]
        );
    }

    #[test]
    fn watches_per_worktree_refs_in_the_linked_git_directory() {
        let git_dir = Path::new("/repo/.git/worktrees/feature");
        let common_dir = Path::new("/repo/.git");

        for reference in ["refs/bisect/good", "refs/worktree/operation"] {
            let paths = git_watch_paths(
                git_dir,
                common_dir,
                Some(&format!("ref: {reference}\n")),
                "refs/notes/homeboy-snapshot",
            );

            assert_eq!(paths.last(), Some(&git_dir.join(reference)));
        }
    }
}
