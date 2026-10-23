//! Small, dependency-light helpers shared by workspace tests.

/// Publish a fixture only after its writer has closed, preserving the requested mode.
///
/// A sibling temporary file keeps the final rename on the same filesystem. Never
/// open the published executable for writing: parallel tests may be executing it.
pub fn write_executable(
    path: impl AsRef<std::path::Path>,
    contents: impl AsRef<[u8]>,
    mode: u32,
) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents.as_ref())?;
    let temporary = temporary.into_temp_path(); // Close before chmod and publication.
    #[cfg(target_os = "linux")]
    wait_for_inherited_writers(&temporary)?;
    #[cfg(unix)]
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
    #[cfg(not(unix))]
    let _ = mode;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

// Closing our writer is not enough if another test forked while it was open:
// the child may retain that descriptor until exec. A read lease is granted only
// once all writers have closed. No new writers can discover this private path.
#[cfg(target_os = "linux")]
fn wait_for_inherited_writers(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        // SAFETY: file owns a valid descriptor; F_SETLEASE takes an integer mode.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLEASE, libc::F_RDLCK) } == 0 {
            // Closing the descriptor releases the lease before publication.
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EAGAIN) || std::time::Instant::now() >= deadline {
            return Err(error);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn publishes_executable_and_replaces_without_rewriting_old_inode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture");
        write_executable(&path, "#!/bin/sh\nprintf old", 0o700).unwrap();
        let old = std::fs::File::open(&path).unwrap();
        write_executable(&path, "#!/bin/sh\nprintf new", 0o751).unwrap();
        assert_ne!(
            old.metadata().unwrap().ino(),
            path.metadata().unwrap().ino()
        );
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o751);
        let output = std::process::Command::new(&path).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"new");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn publication_waits_for_other_writers_to_close() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unpublished");
        let writer = std::fs::File::create(&path).unwrap();
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = closed.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            drop(writer);
        });
        wait_for_inherited_writers(&path).unwrap();
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
        thread.join().unwrap();
    }

    #[test]
    fn failed_publication_cleans_up_temporary_sibling() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("directory");
        std::fs::create_dir(&path).unwrap();
        assert!(write_executable(&path, "#!/bin/sh\n", 0o755).is_err());
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
