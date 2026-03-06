pub fn current_version() -> &'static str {
    VERSION
}

pub(crate) fn fetch_latest_crates_io_version() -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("homeboy/{}", VERSION))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| Error::internal_io(e.to_string(), Some("create HTTP client".to_string())))?;

    let response: CratesIoResponse = client
        .get(CRATES_IO_API)
        .send()
        .map_err(|e| Error::internal_io(e.to_string(), Some("query crates.io".to_string())))?
        .json()
        .map_err(|e| {
            Error::internal_json(e.to_string(), Some("parse crates.io response".to_string()))
        })?;

    Ok(response.crate_info.newest_version)
}

pub(crate) fn fetch_latest_github_version() -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("homeboy/{}", VERSION))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| Error::internal_io(e.to_string(), Some("create HTTP client".to_string())))?;

    let response: GitHubRelease = client
        .get(GITHUB_RELEASES_API)
        .send()
        .map_err(|e| Error::internal_io(e.to_string(), Some("query GitHub releases".to_string())))?
        .json()
        .map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some("parse GitHub release response".to_string()),
            )
        })?;

    // Strip "v" prefix if present (e.g., "v0.15.0" -> "0.15.0")
    let version = response
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&response.tag_name);
    Ok(version.to_string())
}

pub fn fetch_latest_version(method: InstallMethod) -> Result<String> {
    match method {
        InstallMethod::Cargo => fetch_latest_crates_io_version(),
        InstallMethod::Homebrew
        | InstallMethod::Source
        | InstallMethod::Binary
        | InstallMethod::Unknown => fetch_latest_github_version(),
    }
}

pub fn detect_install_method() -> InstallMethod {
    let exe_path = match std::env::current_exe() {
        Ok(path) => path.to_string_lossy().to_string(),
        Err(_) => return InstallMethod::Unknown,
    };

    let defaults = defaults::load_defaults();

    // Check for Homebrew installation via path patterns
    for pattern in &defaults.install_methods.homebrew.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Homebrew;
        }
    }

    // Alternative Homebrew check: brew list (if list_command configured)
    if let Some(list_cmd) = &defaults.install_methods.homebrew.list_command {
        let parts: Vec<&str> = list_cmd.split_whitespace().collect();
        if let Some((cmd, args)) = parts.split_first() {
            if Command::new(cmd)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return InstallMethod::Homebrew;
            }
        }
    }

    // Check for Cargo installation via path patterns
    for pattern in &defaults.install_methods.cargo.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Cargo;
        }
    }

    // Check for source installation via path patterns
    for pattern in &defaults.install_methods.source.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Source;
        }
    }

    // Check for downloaded release binary via path patterns
    for pattern in &defaults.install_methods.binary.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Binary;
        }
    }

    InstallMethod::Unknown
}

pub(crate) fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<(u32, u32, u32)> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() >= 3 {
            Some((
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
            ))
        } else {
            None
        }
    };

    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => latest != current,
    }
}

pub fn run_upgrade_with_method(
    force: bool,
    method_override: Option<InstallMethod>,
) -> Result<UpgradeResult> {
    let install_method = method_override.unwrap_or_else(detect_install_method);
    let previous_version = current_version().to_string();

    if install_method == InstallMethod::Unknown {
        return Err(Error::validation_invalid_argument(
            "install_method",
            "Could not detect installation method",
            None,
            None,
        )
        .with_hint("Try: homeboy upgrade --method binary")
        .with_hint("Or reinstall using: brew install homeboy")
        .with_hint("Or: cargo install homeboy"));
    }

    // Check if update is available (unless forcing)
    if !force {
        let check = check_for_updates()?;
        if !check.update_available {
            return Ok(UpgradeResult {
                command: "upgrade".to_string(),
                install_method,
                previous_version: previous_version.clone(),
                new_version: Some(previous_version),
                upgraded: false,
                message: "Already at latest version".to_string(),
                restart_required: false,
            });
        }
    }

    // Execute the upgrade
    let (success, new_version) = execute_upgrade(install_method)?;

    Ok(UpgradeResult {
        command: "upgrade".to_string(),
        install_method,
        previous_version,
        new_version: new_version.clone(),
        upgraded: success,
        message: if success {
            format!("Upgraded to {}", new_version.as_deref().unwrap_or("latest"))
        } else {
            "Upgrade command completed but version unchanged".to_string()
        },
        restart_required: success,
    })
}

#[cfg(unix)]
pub fn restart_with_new_binary() -> ! {
    use std::os::unix::process::CommandExt;

    // After an upgrade, std::env::current_exe() reads /proc/self/exe which
    // points to the *deleted* old binary inode. Resolve the fresh binary from
    // $PATH instead so we exec into the newly-installed version.
    let binary = resolve_binary_on_path()
        .unwrap_or_else(|| std::env::current_exe().expect("Failed to get current executable path"));

    let err = Command::new(&binary).arg("--version").exec();

    // exec() only returns on error — fall back to a clean exit instead of panicking
    eprintln!(
        "Warning: could not restart automatically ({}). Please run `homeboy --version` to confirm the upgrade.",
        err
    );
    std::process::exit(0);
}
