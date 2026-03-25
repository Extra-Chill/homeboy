use crate::error::Result;

use super::helpers::{
    current_version, detect_install_method, fetch_latest_version, version_is_newer,
};
use super::types::VersionCheck;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_for_updates_default_path() {

        let _result = check_for_updates();
    }

}
