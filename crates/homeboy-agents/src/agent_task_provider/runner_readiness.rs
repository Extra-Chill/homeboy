use super::*;

pub(super) fn resolve_executable_candidate(candidate: &str) -> Option<String> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return None;
    }
    let candidate_path = Path::new(candidate);
    if candidate_path.components().count() > 1 || candidate_path.is_absolute() {
        return executable_file(candidate_path).then(|| candidate.to_string());
    }
    let path_var = std::env::var_os("PATH")?;
    for path in std::env::split_paths(&path_var) {
        let resolved = path.join(candidate);
        if executable_file(&resolved) {
            return Some(resolved.to_string_lossy().to_string());
        }
    }
    None
}

#[cfg(unix)]
pub(super) fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(super) fn executable_file(path: &Path) -> bool {
    path.is_file()
}
