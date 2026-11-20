use super::*;

pub(super) fn rust_cache_identity(cwd: &Path) -> Result<String> {
    let mut hasher = homeboy_engine_primitives::content_hash::sha256_hasher();
    for path in [
        cwd.join("rust-toolchain.toml"),
        cwd.join(".cargo/config.toml"),
        cwd.join(".cargo/config"),
    ] {
        let bytes = fs::read(&path).unwrap_or_default();
        if bytes.is_empty() && !path.is_file() {
            hasher.update(b"absent");
        }
        if path
            .file_name()
            .is_some_and(|name| name == "rust-toolchain.toml")
            && bytes.is_empty()
        {
            return Err(Error::internal_io(
                "required Rust cache identity input is unreadable".to_string(),
                Some(format!("read Rust cache identity {}", path.display())),
            ));
        }
        if !bytes.is_empty() {
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
    }
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update(b"/");
    hasher.update(std::env::consts::ARCH.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn valid_marker_toolchain_cargo(
    marker: &Path,
    identity: &str,
    cache: &Path,
) -> Result<Option<(PathBuf, Option<String>)>> {
    let Ok(bytes) = fs::read(marker) else {
        return Ok(None);
    };
    let marker: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
        Error::internal_io(
            "Rust gate cache marker is corrupt".to_string(),
            Some("validate Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    if marker["schema"] != RUST_CACHE_SCHEMA || marker["identity"] != identity {
        Err(Error::internal_io(
            "Rust gate cache marker does not match its content address".to_string(),
            Some("validate Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command()))
    } else {
        let Some(relative) = marker["toolchain_cargo_relative"].as_str() else {
            return Ok(None);
        };
        let relative = PathBuf::from(relative);
        if relative.is_absolute()
            || !relative.starts_with("toolchains")
            || !cache.join("rustup").join(&relative).is_file()
        {
            return Ok(None);
        }
        Ok(Some((
            relative,
            marker["lockfile_hash"].as_str().map(str::to_owned),
        )))
    }
}

pub(super) fn hydrate_rust_cache_with_timeout(
    cwd: &Path,
    cache: &Path,
    timeout: Duration,
    cached_toolchain_cargo: Option<&Path>,
) -> Result<PathBuf> {
    let environment = BTreeMap::from([
        ("HOME".to_string(), cache.join("home").display().to_string()),
        (
            "CARGO_HOME".to_string(),
            cache.join("cargo").display().to_string(),
        ),
        (
            "RUSTUP_HOME".to_string(),
            cache.join("rustup").display().to_string(),
        ),
    ]);
    let cargo = if let Some(relative) = cached_toolchain_cargo {
        cache.join("rustup").join(relative)
    } else {
        // The host rustup proxy refuses to run after CARGO_HOME is isolated unless
        // the proxy itself lives under that new home. Seed the cache with a real
        // file rather than inheriting the host home into controller-owned state.
        let rustup_proxy = prepare_rustup_proxy(cache)?;
        if !share_host_toolchain(cwd, cache, &rustup_proxy, timeout)? {
            let rustup = run_rust_cache_command(
                cwd,
                cache,
                &environment,
                "install_toolchain",
                &rustup_proxy,
                &["toolchain", "install"],
                timeout,
            )?;
            drop(rustup);
        }
        let resolved = run_rust_cache_command(
            cwd,
            cache,
            &environment,
            "resolve_toolchain_cargo",
            &rustup_proxy,
            &["which", "cargo"],
            timeout,
        )?;
        PathBuf::from(String::from_utf8_lossy(&resolved.stdout).trim())
    };
    let rustup_home = cache.join("rustup");
    let relative = cargo.strip_prefix(&rustup_home).map_err(|_| {
        Error::internal_io(
            format!(
                "Rust cache hydration resolved Cargo outside isolated RUSTUP_HOME: {}",
                cargo.display()
            ),
            Some("resolve_toolchain_cargo".to_string()),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    if !relative.starts_with("toolchains") || !cargo.is_file() {
        return Err(Error::internal_io(
            format!(
                "Rust cache hydration resolved an invalid toolchain Cargo executable: {}",
                cargo.display()
            ),
            Some("resolve_toolchain_cargo".to_string()),
        )
        .with_hint(rust_cache_repair_command()));
    }
    run_rust_cache_command(
        cwd,
        cache,
        &environment,
        "fetch_locked",
        &cargo,
        &["fetch", "--locked"],
        timeout,
    )?;
    Ok(relative.to_path_buf())
}

/// Seed rustup with the runner's installed toolchain, when available. Resolve
/// using the host homes but the checkout's toolchain selection; installation
/// and all subsequent writable state remain isolated.
pub(super) fn share_host_toolchain(
    cwd: &Path,
    cache: &Path,
    proxy: &Path,
    timeout: Duration,
) -> Result<bool> {
    let environment = ["HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN"]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name.to_string(), value))
        })
        .collect();
    let Ok(output) = run_rust_cache_command(
        cwd,
        cache,
        &environment,
        "resolve_host_toolchain",
        proxy,
        &["which", "cargo"],
        timeout,
    ) else {
        return Ok(false);
    };
    let cargo = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let Some(toolchain) = cargo.parent().and_then(Path::parent) else {
        return Ok(false);
    };
    // Only trust a complete, installed toolchain, not arbitrary proxy output.
    if !cargo.is_absolute() || !cargo.is_file() || !toolchain.join("bin/rustc").is_file() {
        return Ok(false);
    }
    let Some(name) = toolchain.file_name() else {
        return Ok(false);
    };
    let directory = cache.join("rustup/toolchains");
    fs::create_dir_all(&directory)
        .and_then(|()| create_rust_cache_symlink(toolchain, &directory.join(name), true))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("share host Rust toolchain".to_string()),
            )
        })?;
    Ok(true)
}

/// Copy writable rustup state, but share each immutable installed toolchain.
/// Canonical targets keep overlays independent of cache identity eviction.
pub(super) fn hydrate_gate_rustup(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination).map_err(|error| Error::internal_io(error.to_string(), None))?;
    for entry in
        fs::read_dir(source).map_err(|error| Error::internal_io(error.to_string(), None))?
    {
        let entry = entry.map_err(|error| Error::internal_io(error.to_string(), None))?;
        let target = destination.join(entry.file_name());
        if entry.file_name() == "toolchains" {
            fs::create_dir_all(&target)
                .map_err(|error| Error::internal_io(error.to_string(), None))?;
            for toolchain in fs::read_dir(entry.path())
                .map_err(|error| Error::internal_io(error.to_string(), None))?
            {
                let toolchain =
                    toolchain.map_err(|error| Error::internal_io(error.to_string(), None))?;
                let resolved = toolchain
                    .path()
                    .canonicalize()
                    .map_err(|error| Error::internal_io(error.to_string(), None))?;
                let destination = target.join(toolchain.file_name());
                if toolchain
                    .file_type()
                    .map_err(|error| Error::internal_io(error.to_string(), None))?
                    .is_symlink()
                {
                    create_rust_cache_symlink(&resolved, &destination, true)
                        .map_err(|error| Error::internal_io(error.to_string(), None))?;
                } else {
                    // Cache-owned installs can be evicted while the gate runs.
                    // Hardlinks retain their bytes without duplicating them.
                    copy_rust_cache_tree(&resolved, &destination, &resolved, &destination, true)?;
                }
            }
        } else if entry
            .file_type()
            .map_err(|error| Error::internal_io(error.to_string(), None))?
            .is_dir()
        {
            copy_tree_preserving_safe_symlinks(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)
                .map_err(|error| Error::internal_io(error.to_string(), None))?;
        }
    }
    Ok(())
}

/// Find the host `rustup` to proxy into the isolated gate cache.
///
/// `PATH` first, then rustup's own install locations: `$CARGO_HOME/bin` and
/// `$HOME/.cargo/bin`. A daemon or service started outside a login shell
/// usually lacks `~/.cargo/bin` on `PATH` even though rustup is installed
/// there, which failed every Rust gate on such hosts.
pub(super) fn locate_host_rustup(
    path: Option<&std::ffi::OsStr>,
    cargo_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let path_dirs = path
        .map(|path| std::env::split_paths(path).collect::<Vec<_>>())
        .unwrap_or_default();
    let install_dirs = cargo_home
        .filter(|value| !value.is_empty())
        .map(|cargo_home| PathBuf::from(cargo_home).join("bin"))
        .into_iter()
        .chain(
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".cargo").join("bin")),
        );
    path_dirs
        .into_iter()
        .chain(install_dirs)
        .map(|directory| directory.join("rustup"))
        .find(|candidate| candidate.is_file())
}

pub(super) fn prepare_rustup_proxy(cache: &Path) -> Result<PathBuf> {
    let source = locate_host_rustup(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("CARGO_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "rustup",
            "unavailable for Rust gate cache hydration: not found on PATH, in $CARGO_HOME/bin, or in ~/.cargo/bin",
            None,
            None,
        )
    })?;
    let destination = cache.join("cargo/bin/rustup");
    fs::create_dir_all(destination.parent().expect("rustup proxy has a parent")).map_err(
        |error| {
            Error::internal_io(
                error.to_string(),
                Some("prepare isolated Rust gate cache rustup proxy".to_string()),
            )
        },
    )?;
    fs::copy(&source, &destination).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("prepare isolated Rust gate cache rustup proxy".to_string()),
        )
    })?;
    Ok(destination)
}

pub(super) fn run_rust_cache_command(
    cwd: &Path,
    cache: &Path,
    environment: &BTreeMap<String, String>,
    phase: &str,
    program: &Path,
    arguments: &[&str],
    timeout: Duration,
) -> Result<std::process::Output> {
    let mut process = Command::new(program);
    process
        .current_dir(cwd)
        .args(arguments)
        .env_clear()
        .envs(environment)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let host_path = std::env::var_os("PATH").ok_or_else(|| {
        Error::validation_invalid_argument(
            "PATH",
            "unavailable for Rust gate cache hydration",
            None,
            None,
        )
    })?;
    // A cache-owned executable must resolve its siblings from its own
    // directory first: toolchain `cargo` spawns `rustc` by name, and the host
    // PATH may hold no rustc at all (#15453). Gate execution prepends the same
    // toolchain bin; hydration has to match it.
    let path = std::env::join_paths(
        program
            .parent()
            .map(Path::to_path_buf)
            .into_iter()
            .chain(std::env::split_paths(&host_path)),
    )
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("Rust gate cache hydration phase {phase} PATH")),
        )
    })?;
    process.env("PATH", path);
    homeboy_core::engine::command::isolate_process_tree(&mut process);
    let mut child = process.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("Rust gate cache hydration phase {phase}")),
        )
    })?;
    let started = Instant::now();
    let mut timed_out = false;
    let output = homeboy_core::engine::command::wait_with_bounded_output_until_cancelled(
        &mut child,
        GATE_TOOLCHAIN_CAPTURE_LIMIT_BYTES,
        || {
            timed_out = started.elapsed() >= timeout;
            timed_out
        },
    )
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("hydrate Rust gate cache".to_string()),
        )
    })?
    .into_output();
    if timed_out || !output.status.success() {
        return Err(Error::internal_io(
            format!(
                "Rust gate cache hydration phase {phase} {} (cache {}, command `{} {}`): {}",
                if timed_out { "timed out" } else { "failed" },
                cache.display(),
                program.display(),
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(format!("Rust gate cache hydration phase {phase}")),
        )
        .with_hint(rust_cache_repair_command()));
    }
    Ok(output)
}

/// Clone controller-owned cache bytes into a gate-owned overlay. Links may only
/// resolve within the authoritative source tree, and absolute links are rebased
/// so the gate never retains a path back into controller-owned cache state.
pub(super) fn copy_tree_preserving_safe_symlinks(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("inspect Rust cache {}", source.display())),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::internal_io(
            "Rust cache contains an unsafe root".to_string(),
            Some("copy Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command()));
    }
    let source_root = source.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("resolve Rust cache root {}", source.display())),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    copy_rust_cache_tree(source, destination, &source_root, destination, false)
}

pub(super) fn copy_rust_cache_tree(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
    hardlink_files: bool,
) -> Result<()> {
    fs::create_dir_all(destination).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create Rust overlay {}", destination.display())),
        )
    })?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read Rust cache {}", source.display())),
            )
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("read Rust cache entry".to_string()))
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("inspect Rust cache entry".to_string()),
            )
        })?;
        if file_type.is_symlink() {
            copy_safe_rust_cache_symlink(
                &source_path,
                &destination_path,
                source_root,
                destination_root,
            )?;
        } else if file_type.is_dir() {
            copy_rust_cache_tree(
                &source_path,
                &destination_path,
                source_root,
                destination_root,
                hardlink_files,
            )?;
        } else if file_type.is_file() {
            let linked = if hardlink_files {
                match fs::hard_link(&source_path, &destination_path) {
                    Ok(()) => true,
                    Err(error) if error.raw_os_error() == Some(libc::EXDEV) => false,
                    Err(error) => {
                        return Err(Error::internal_io(
                            error.to_string(),
                            Some("hardlink Rust toolchain".to_string()),
                        ))
                    }
                }
            } else {
                false
            };
            if linked {
                continue;
            }
            fs::copy(&source_path, &destination_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("copy Rust gate cache entry".to_string()),
                )
            })?;
        } else {
            return Err(Error::internal_io(
                "Rust cache contains an unsupported file type".to_string(),
                Some("copy Rust gate cache".to_string()),
            )
            .with_hint(rust_cache_repair_command()));
        }
    }
    Ok(())
}

pub(super) fn copy_safe_rust_cache_symlink(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
) -> Result<()> {
    let target = fs::read_link(source).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read Rust cache symlink {}", source.display())),
        )
    })?;
    let resolved = source.canonicalize().map_err(|error| {
        let kind = if error.raw_os_error() == Some(libc::ELOOP) {
            "cycle"
        } else if error.kind() == std::io::ErrorKind::NotFound {
            "dangling target"
        } else {
            "unresolvable target"
        };
        Error::internal_io(
            format!(
                "Rust cache symlink {} has a {kind}: {}",
                source.display(),
                target.display()
            ),
            Some("copy Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    let relative = resolved.strip_prefix(source_root).map_err(|_| {
        Error::internal_io(
            format!(
                "Rust cache symlink {} escapes its source root: {}",
                source.display(),
                resolved.display()
            ),
            Some("copy Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    let overlay_target = if target.is_absolute() {
        destination_root.join(relative)
    } else {
        target
    };
    create_rust_cache_symlink(&overlay_target, destination, resolved.is_dir()).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("copy Rust cache symlink {}", source.display())),
        )
    })
}

#[cfg(unix)]
pub(super) fn create_rust_cache_symlink(
    target: &Path,
    link: &Path,
    _target_is_dir: bool,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
pub(super) fn create_rust_cache_symlink(
    target: &Path,
    link: &Path,
    target_is_dir: bool,
) -> std::io::Result<()> {
    if target_is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

pub(super) fn rust_cache_repair_command() -> String {
    "rm -rf \"$(homeboy paths data)/controller-state/gate-rust-cache\"".to_string()
}

/// Create `path` (and missing parents) with the leaf directory private to
/// the current user. Plain `create_dir_all` follows the process umask, so on a
/// host with umask 0002 (stock Ubuntu) the cache came out group-writable and
/// was then refused by the ownership check below (Extra-Chill/homeboy#15393).
/// `umask` can only remove bits, so creating with 0700 yields 0700 or less.
pub(super) fn create_private_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => Err(error),
            _ => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Self-contained so an operator can act on it even when hints are not shown.
pub(super) fn unsafe_rust_cache_root_message(
    path: &Path,
    owner_uid: u32,
    mode: u32,
    expected_uid: u32,
) -> String {
    format!(
        "Rust gate cache root {} has unsafe ownership or permissions: owned by uid {} with mode {:o}, expected uid {} and no group/other write; repair with: {}",
        path.display(),
        owner_uid,
        mode & 0o7777,
        expected_uid,
        rust_cache_repair_command()
    )
}

/// How many Rust gate cache identities to keep. Each is a full Cargo and
/// rustup home (~2 GB for Homeboy itself) keyed by `Cargo.lock` and
/// `rust-toolchain.toml`, and a lockfile changes on nearly every release, so
/// an unbounded cache gained gigabytes a day (Extra-Chill/homeboy#15447).
pub(super) const RUST_CACHE_RETAINED_IDENTITIES: usize = 2;

/// Remove all but the `keep` most recently used identity directories under
/// `root`, never `current`. Recency is the mtime of the identity's
/// `ready.json` marker, written once when it is hydrated; the directory's own
/// mtime is unusable because opening `hydrate.lock` creates or touches an
/// entry in it. An identity without a marker (never finished hydrating) sorts
/// oldest. An identity is removed only while this
/// process holds its `hydrate.lock`, so a concurrent hydration is never cut
/// short; a gate reading a hydrated identity copies it into its own overlay
/// first, so removing it afterwards is safe. Returns the removed identities.
pub(super) fn evict_stale_rust_cache_identities(
    root: &Path,
    current: &str,
    keep: usize,
) -> Result<Vec<String>> {
    let entries = fs::read_dir(root).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("list {}", root.display())))
    })?;
    let mut identities: Vec<(std::time::SystemTime, String, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.path().symlink_metadata().ok()?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            let used = fs::metadata(entry.path().join("ready.json"))
                .and_then(|marker| marker.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            Some((used, name, entry.path()))
        })
        .collect();
    identities.sort_by(|a, b| b.0.cmp(&a.0));
    let mut kept = usize::from(identities.iter().any(|(_, name, _)| name == current));
    let mut removed = Vec::new();
    for (_, name, path) in identities {
        if name == current {
            continue;
        }
        if kept < keep {
            kept += 1;
            continue;
        }
        let Ok(lock) = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path.join("hydrate.lock"))
        else {
            continue;
        };
        if !matches!(lock.try_lock_exclusive(), Ok(true)) {
            continue;
        }
        if fs::remove_dir_all(&path).is_ok() {
            removed.push(name);
        }
    }
    Ok(removed)
}

pub(super) fn ensure_safe_rust_cache_root(path: &Path) -> Result<()> {
    create_private_dir(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", path.display())),
        )
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("inspect {}", path.display())),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::internal_io(
            "Rust gate cache root is not a real directory".to_string(),
            Some("validate Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid || metadata.permissions().mode() & 0o022 != 0 {
            return Err(Error::internal_io(
                unsafe_rust_cache_root_message(
                    path,
                    metadata.uid(),
                    metadata.permissions().mode(),
                    euid,
                ),
                Some("validate Rust gate cache".to_string()),
            )
            .with_hint(rust_cache_repair_command()));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("secure {}", path.display())),
            )
        })?;
    }
    Ok(())
}
