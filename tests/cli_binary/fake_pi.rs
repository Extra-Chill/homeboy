use std::path::{Path, PathBuf};

/// Install a fake Pi executable on an offline-GitHub PATH; previews must not
/// create Homeboy state.
pub(super) fn install(home: &Path, models: &str) -> PathBuf {
    write_executable(
        &home.join("bin").join("pi"),
        &format!("#!/bin/sh\ncase \"$1\" in\n--version) printf 'test-pi\\n';;\n--list-models) printf '%s\\n' '{models}';;\n*) exit 99;;\nesac\n"),
    );
    offline_github(home)
}

/// PATH whose `gh` reports an issue timeline with no linked PRs, so Cook
/// issue resolution never reaches the network.
pub(super) fn offline_github(home: &Path) -> PathBuf {
    let bin = home.join("bin");
    write_executable(
        &bin.join("gh"),
        "#!/bin/sh\ncase \"$1\" in\napi) printf '[[]]';;\n*) exit 1;;\nesac\n",
    );
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap()
    .into()
}

fn write_executable(path: &Path, script: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
