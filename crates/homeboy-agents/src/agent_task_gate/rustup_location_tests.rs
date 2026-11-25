use super::locate_host_rustup;

fn rustup_in(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let rustup = dir.join("rustup");
    std::fs::write(&rustup, b"#!/bin/sh\n").unwrap();
    rustup
}

#[test]
fn rustup_lookup_prefers_path_then_cargo_home_then_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cargo_home = temp.path().join("custom-cargo");
    let empty_path = std::env::join_paths([temp.path().join("empty")]).unwrap();
    assert_eq!(
        locate_host_rustup(Some(&empty_path), None, Some(home.as_os_str())),
        None
    );

    let home_rustup = rustup_in(&home.join(".cargo/bin"));
    assert_eq!(
        locate_host_rustup(Some(&empty_path), None, Some(home.as_os_str())),
        Some(home_rustup),
        "a daemon without ~/.cargo/bin on PATH still finds rustup"
    );

    let custom = rustup_in(&cargo_home.join("bin"));
    assert_eq!(
        locate_host_rustup(
            Some(&empty_path),
            Some(cargo_home.as_os_str()),
            Some(home.as_os_str())
        ),
        Some(custom),
        "$CARGO_HOME takes precedence over ~/.cargo"
    );

    let on_path = rustup_in(&temp.path().join("path-bin"));
    let path = std::env::join_paths([temp.path().join("path-bin")]).unwrap();
    assert_eq!(
        locate_host_rustup(
            Some(&path),
            Some(cargo_home.as_os_str()),
            Some(home.as_os_str())
        ),
        Some(on_path),
        "PATH wins over install locations"
    );
}
