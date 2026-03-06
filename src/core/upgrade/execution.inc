pub(crate) fn execute_upgrade(method: InstallMethod) -> Result<(bool, Option<String>)> {
    let defaults = defaults::load_defaults();

    let output = match method {
        InstallMethod::Homebrew => {
            let cmd = &defaults.install_methods.homebrew.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run homebrew upgrade".to_string()))
            })?
        }
        InstallMethod::Cargo => {
            let cmd = &defaults.install_methods.cargo.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run cargo upgrade".to_string()))
            })?
        }
        InstallMethod::Source => {
            // For source builds, we need to find the git root
            let exe_path = std::env::current_exe().map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some("get current executable path".to_string()),
                )
            })?;

            // Navigate up from target/release/homeboy to find the workspace root
            let mut workspace_root = exe_path.clone();
            for _ in 0..3 {
                workspace_root = workspace_root
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or(workspace_root);
            }

            // Check if this looks like a git repo
            let git_dir = workspace_root.join(".git");
            if !git_dir.exists() {
                return Err(Error::validation_invalid_argument(
                    "source_path",
                    "Could not find git repository for source build",
                    Some(workspace_root.to_string_lossy().to_string()),
                    None,
                ));
            }

            // Execute the upgrade command from defaults
            let cmd = &defaults.install_methods.source.upgrade_command;
            Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&workspace_root)
                .output()
                .map_err(|e| {
                    Error::internal_io(e.to_string(), Some("run source upgrade".to_string()))
                })?
        }
        InstallMethod::Binary => {
            let cmd = &defaults.install_methods.binary.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run binary upgrade".to_string()))
            })?
        }
        InstallMethod::Unknown => {
            return Err(Error::validation_invalid_argument(
                "install_method",
                "Cannot upgrade: unknown installation method",
                None,
                None,
            ));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let error_detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("exit code {}", output.status.code().unwrap_or(1))
        };
        return Err(Error::internal_io(
            format!("{} upgrade failed: {}", method.as_str(), error_detail),
            Some("execute upgrade".to_string()),
        ));
    }

    // Try to fetch the new version
    let new_version = fetch_latest_version(method).ok();

    Ok((true, new_version))
}
