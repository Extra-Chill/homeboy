use std::path::{Path, PathBuf};

/// Install only a fake Pi executable; previews must not create Homeboy state.
pub(super) fn install(home: &Path, models: &str) -> PathBuf {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let command = bin.join("pi");
    std::fs::write(&command, format!("#!/bin/sh\ncase \"$1\" in\n--version) printf 'test-pi\\n';;\n--list-models) printf '%s\\n' '{models}';;\n*) exit 99;;\nesac\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(command, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap()
    .into()
}
