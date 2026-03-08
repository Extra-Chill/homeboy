pub fn check_for_updates() -> Result<VersionCheck> {
    let install_method = detect_install_method();
    let current = current_version().to_string();

    let latest = fetch_latest_version(install_method).ok();
    let update_available = latest
        .as_ref()
        .map(|l| version_is_newer(l, &current))
        .unwrap_or(false);

    Ok(VersionCheck {
        command: "upgrade.check".to_string(),
        current_version: current,
        latest_version: latest,
        update_available,
        install_method,
    })
}
