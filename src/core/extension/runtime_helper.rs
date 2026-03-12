use crate::error::{Error, Result};
use crate::paths;
use crate::local_files;
use std::fs;
use std::path::PathBuf;

const RUNNER_STEPS_SH: &str = include_str!("runtime/runner-steps.sh");

pub const RUNNER_STEPS_ENV: &str = "HOMEBOY_RUNTIME_RUNNER_STEPS";

pub fn ensure_runner_steps_helper() -> Result<PathBuf> {
    let runtime_dir = paths::homeboy()?.join("runtime");
    fs::create_dir_all(&runtime_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("create homeboy runtime directory".to_string()),
        )
    })?;

    let helper_path = runtime_dir.join("runner-steps.sh");
    let current = fs::read_to_string(&helper_path).ok();

    if current.as_deref() != Some(RUNNER_STEPS_SH) {
        local_files::write_file_atomic(&helper_path, RUNNER_STEPS_SH, "write runtime runner helper")?;
    }

    Ok(helper_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_runner_steps_helper_writes_expected_contents() {
        let path = ensure_runner_steps_helper().expect("helper should be written");
        let contents = std::fs::read_to_string(&path).expect("helper should be readable");
        assert_eq!(contents, RUNNER_STEPS_SH);
        assert!(path.ends_with("runner-steps.sh"));
    }
}
