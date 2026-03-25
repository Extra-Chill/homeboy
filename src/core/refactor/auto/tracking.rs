use std::collections::HashSet;

#[cfg(not(test))]
pub(crate) fn changed_file_set(local_path: &str) -> crate::Result<HashSet<String>> {
    let uncommitted = crate::git::get_uncommitted_changes(local_path)?;
    let mut files = HashSet::new();
    files.extend(uncommitted.staged);
    files.extend(uncommitted.unstaged);
    files.extend(uncommitted.untracked);
    Ok(files)
}

#[cfg(test)]
pub fn changed_file_set(local_path: &str) -> crate::Result<HashSet<String>> {
    let path = std::path::Path::new(local_path);
    if path.exists() {
        Ok(HashSet::new())
    } else {
        crate::git::get_uncommitted_changes(local_path).map(|changes| {
            let mut files = HashSet::new();
            files.extend(changes.staged);
            files.extend(changes.unstaged);
            files.extend(changes.untracked);
            files
        })
    }
}

pub(crate) fn count_newly_changed(before: &HashSet<String>, after: &HashSet<String>) -> usize {
    after.difference(before).count()
}

pub(crate) fn newly_changed_files(before: &HashSet<String>, after: &HashSet<String>) -> Vec<String> {
    let mut changed: Vec<String> = after.difference(before).cloned().collect();
    changed.sort();
    changed
}
