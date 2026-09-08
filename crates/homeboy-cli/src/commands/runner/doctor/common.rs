use super::*;

/// Result of resolving an executable name against `PATH`.
///
/// The three cases are kept apart on purpose. A probe that cannot run has not
/// proven the tool absent, and collapsing the two is what made `runner doctor`
/// report a working `git` as missing (#14374).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathLookup {
    Found(String),
    NotFound,
    /// The lookup could not be performed at all.
    Unavailable(String),
}

/// Resolve `command` against `PATH` without invoking a shell.
///
/// Tool presence is a property of the filesystem and the process environment,
/// not of the operator's interactive profile. The previous implementation shelled
/// out to `sh -lc "command -v <tool>"`, so any error in `~/.profile` (a stale
/// rustup/nvm/conda `.` line is the common case) made dash exit non-zero before
/// `command -v` ever ran, and every tool on the host read as absent.
pub(crate) fn resolve_on_path(command: &str) -> PathLookup {
    resolve_in_path(command, env::var_os("PATH").as_deref())
}

/// `resolve_on_path` with the search path supplied explicitly, so the walk can
/// be exercised without mutating the process environment.
pub(crate) fn resolve_in_path(command: &str, path_var: Option<&OsStr>) -> PathLookup {
    if command.contains('/') || command.contains(std::path::MAIN_SEPARATOR) {
        // `command -v` treats a name containing a separator as a path to test,
        // never as a PATH lookup. Match that.
        return match executable_path(Path::new(command)) {
            Some(path) => PathLookup::Found(path),
            None => PathLookup::NotFound,
        };
    }

    let Some(path_var) = path_var else {
        return PathLookup::Unavailable("PATH is not set for the doctor process".to_string());
    };
    if path_var.is_empty() {
        return PathLookup::Unavailable("PATH is empty for the doctor process".to_string());
    }

    for directory in env::split_paths(path_var) {
        // POSIX reads an empty PATH entry as the working directory. Resolving a
        // runner tool relative to wherever doctor happened to be invoked is not
        // a readiness signal, so those entries are skipped.
        if directory.as_os_str().is_empty() {
            continue;
        }
        if let Some(path) = executable_path(&directory.join(command)) {
            return PathLookup::Found(path);
        }
    }

    PathLookup::NotFound
}

fn executable_path(candidate: &Path) -> Option<String> {
    // `metadata` follows symlinks, so a symlinked tool resolves like it does
    // for exec.
    let metadata = fs::metadata(candidate).ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(display_path(candidate))
}

pub(crate) fn local_command_line(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    first_output_line(&output.stdout, &output.stderr)
}

/// Resolve `command` on a remote runner, keeping "shell failed" apart from
/// "tool absent".
///
/// `command -v` exits non-zero and stays silent when the tool is genuinely
/// missing. Anything written to stderr means the remote shell had its own
/// problem — a broken profile line, a missing interpreter — and the lookup never
/// reached a verdict about the tool.
pub(crate) fn remote_path_lookup(client: &SshClient, command: &str) -> PathLookup {
    let output = client.execute(&format!("command -v {}", shell_word(command)));
    if output.success {
        return match first_nonempty_line(&output.stdout) {
            Some(path) => PathLookup::Found(path),
            None => PathLookup::NotFound,
        };
    }
    match first_nonempty_line(&output.stderr) {
        Some(stderr) => PathLookup::Unavailable(stderr),
        None => PathLookup::NotFound,
    }
}

fn first_nonempty_line(value: &str) -> Option<String> {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

pub(crate) fn remote_line(client: &SshClient, command: &str) -> Option<String> {
    let output = client.execute(command);
    if !output.success {
        return None;
    }
    first_nonempty_line(&output.stdout)
}

pub(crate) fn first_output_line(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let combined = if stdout.is_empty() { stderr } else { stdout };
    String::from_utf8_lossy(combined)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

pub fn display_path(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().to_string()
}

pub fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn shell_path_expr(path: &str) -> String {
    if path == "~" {
        return "\"${HOME}\"".to_string();
    }

    if let Some(rest) = path.strip_prefix("~/") {
        return format!("\"${{HOME}}\"/{}", shell_word(rest));
    }

    shell_word(path)
}

pub(crate) fn detail_map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}
