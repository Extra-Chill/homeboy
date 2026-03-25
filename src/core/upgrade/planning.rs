/// Resolve the `homeboy` binary via $PATH, returning the first match that
/// exists on disk. This avoids the stale `/proc/self/exe` problem on Linux
/// where the old inode is deleted after the upgrade replaces the binary.
pub fn resolve_binary_on_path() -> Option<std::path::PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    resolve_binary_on_path_var(&path_var)
}

pub(crate) fn resolve_binary_on_path_var(path_var: &str) -> Option<std::path::PathBuf> {
    for dir in path_var.split(':') {
        let candidate = std::path::PathBuf::from(dir).join("homeboy");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_binary_on_path_default_path() {

        let _result = resolve_binary_on_path();
    }

    #[test]
    fn test_resolve_binary_on_path_var_candidate_exists() {

        let result = resolve_binary_on_path_var();
        assert!(result.is_some(), "expected Some for: candidate.exists()");
    }

    #[test]
    fn test_resolve_binary_on_path_var_candidate_exists_2() {

        let result = resolve_binary_on_path_var();
        assert!(result.is_none(), "expected None for: candidate.exists()");
    }

}
