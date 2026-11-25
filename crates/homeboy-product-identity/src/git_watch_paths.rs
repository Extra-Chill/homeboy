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

    /// Shared refs (branches, notes, packed-refs) live in the common git dir
    /// even for a linked worktree; HEAD and per-worktree refs (bisect,
    /// worktree) live in the worktree's own git dir; a detached HEAD adds no
    /// symbolic ref to watch.
    #[test]
    fn watches_each_ref_in_the_git_directory_that_owns_it() {
        let common = "/repo/.git";
        let linked = "/repo/.git/worktrees/feature";
        let cases = [
            (
                common,
                "ref: refs/heads/main\n",
                Some("/repo/.git/refs/heads/main"),
            ),
            (
                linked,
                "ref: refs/heads/feature\n",
                Some("/repo/.git/refs/heads/feature"),
            ),
            (linked, "9e102eb708131db18e55e100cc47262694148056\n", None),
            (
                linked,
                "ref: refs/bisect/good\n",
                Some("/repo/.git/worktrees/feature/refs/bisect/good"),
            ),
            (
                linked,
                "ref: refs/worktree/operation\n",
                Some("/repo/.git/worktrees/feature/refs/worktree/operation"),
            ),
        ];

        for (git_dir, head, symbolic) in cases {
            let paths = git_watch_paths(
                Path::new(git_dir),
                Path::new(common),
                Some(head),
                "refs/notes/homeboy-snapshot",
            );

            let mut expected = vec![
                Path::new(git_dir).join("HEAD"),
                PathBuf::from("/repo/.git/packed-refs"),
                PathBuf::from("/repo/.git/refs/notes/homeboy-snapshot"),
            ];
            expected.extend(symbolic.map(PathBuf::from));
            assert_eq!(paths, expected, "HEAD {head:?} in {git_dir}");
        }
    }
}
