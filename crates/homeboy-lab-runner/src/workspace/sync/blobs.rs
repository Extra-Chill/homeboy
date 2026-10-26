//! Plain, verified SHA-256 files shared by materialization stages.
use super::protocol::io;
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::time::{Duration, SystemTime};

pub(in crate::workspace) fn store(remote: &str) -> PathBuf {
    let parent = Path::new(remote).parent().unwrap();
    let root = if parent.file_name().is_some_and(|n| n == "_lab_workspaces") {
        parent.parent().unwrap()
    } else {
        parent
    };
    root.join(".homeboy-blobs")
}

pub(in crate::workspace) fn key(digest: &str) -> Result<&str> {
    let key = digest.strip_prefix("sha256:").unwrap_or("");
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io("invalid blob digest"));
    }
    Ok(key)
}

pub(in crate::workspace) fn valid(
    path: &Path,
    digest: &str,
    control: &WorkspaceControl,
) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    Ok(super::super::snapshot::snapshot_file_sha256(path, control)? == key(digest)?)
}

pub(super) fn inventory(roots: &[protocol::RootSummary]) -> Result<BTreeMap<String, u64>> {
    let mut blobs = BTreeMap::new();
    for root in roots {
        for entry in &root.manifest.entries {
            if entry.kind == "file" {
                let digest = entry
                    .sha256
                    .as_ref()
                    .ok_or_else(|| io("file has no digest"))?;
                key(digest)?;
                blobs.insert(digest.clone(), entry.bytes.unwrap_or(0));
            }
        }
    }
    Ok(blobs)
}

pub(super) fn assembled_size(root: &protocol::RootSummary) -> SnapshotFilesystemRequirement {
    let mut links = Vec::<PathBuf>::new();
    let mut size = SnapshotFilesystemRequirement {
        bytes: 0,
        inodes: 0,
    };
    for entry in &root.manifest.entries {
        let path = Path::new(&entry.path);
        if links.iter().any(|link| path.starts_with(link)) {
            continue;
        }
        size.inodes += 1;
        if entry.symlink_target.is_some() {
            links.push(path.to_path_buf());
        } else if entry.kind == "file" {
            size.bytes += entry.bytes.unwrap_or(0);
        }
    }
    size
}

pub(super) fn assemble(
    root: &protocol::RootSummary,
    destination: &Path,
    control: &WorkspaceControl,
) -> Result<()> {
    let store = store(&root.remote_path);
    fs::create_dir_all(destination).map_err(io)?;
    let mut links = Vec::<PathBuf>::new();
    for entry in &root.manifest.entries {
        control.checkpoint()?;
        let relative = Path::new(&entry.path);
        if relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || entry.path.is_empty()
            || relative.starts_with(".git")
        {
            return Err(io("unsafe manifest path"));
        }
        if links.iter().any(|link| relative.starts_with(link)) {
            continue;
        }
        let target = destination.join(relative);
        fs::create_dir_all(target.parent().unwrap()).map_err(io)?;
        if let Some(link) = &entry.symlink_target {
            #[cfg(unix)]
            std::os::unix::fs::symlink(link, &target).map_err(io)?;
            #[cfg(not(unix))]
            return Err(io("symlink materialization requires Unix"));
            links.push(relative.to_path_buf());
        } else if entry.kind == "directory" {
            fs::create_dir_all(&target).map_err(io)?;
        } else if entry.kind == "file" {
            let digest = entry
                .sha256
                .as_deref()
                .ok_or_else(|| io("missing file digest"))?;
            let blob = store.join(key(digest)?);
            if !valid(&blob, digest, control)? {
                let _ = fs::remove_file(&blob);
                return Err(io("blob corruption during assembly; retry plan to refetch"));
            }
            // Execution views are mutable: sharing an inode with the cache
            // would let a build corrupt later stages (including chmod).
            fs::copy(&blob, &target).map_err(io)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &target,
                    fs::Permissions::from_mode(entry.mode.unwrap_or(0o644)),
                )
                .map_err(io)?;
            }
            fs::File::open(&blob)
                .map_err(io)?
                .set_modified(SystemTime::now())
                .map_err(io)?;
        } else {
            return Err(io("unsupported manifest entry"));
        }
    }
    Ok(())
}

/// LRU by mtime; called by workspace retention and before planning. Only
/// complete digest-named files participate, never in-flight uploads.
pub(in crate::workspace) fn prune(store: &Path) -> Result<()> {
    let Ok(entries) = fs::read_dir(store) else {
        return Ok(());
    };
    let mut entries = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let m = e.metadata().ok()?;
            (m.is_file()).then(|| {
                (
                    m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    m.len(),
                    e.path(),
                )
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|e| e.0);
    let mut bytes: u64 = entries.iter().map(|e| e.1).sum();
    for (mtime, size, path) in entries {
        if bytes > 10 * 1024 * 1024 * 1024
            || mtime.elapsed().unwrap_or_default() > Duration::from_secs(7 * 86400)
        {
            fs::remove_file(path).map_err(io)?;
            bytes = bytes.saturating_sub(size);
        }
    }
    Ok(())
}

pub(in crate::workspace) struct Source {
    pub(in crate::workspace) path: PathBuf,
    pub(in crate::workspace) digest: String,
    pub(in crate::workspace) metadata: fs::Metadata,
}

impl Source {
    pub(in crate::workspace) fn new(path: PathBuf, digest: String) -> Result<Self> {
        let metadata = fs::metadata(&path).map_err(io)?;
        Ok(Self {
            path,
            digest,
            metadata,
        })
    }

    pub(in crate::workspace) fn append(
        &self,
        archive: &mut tar::Builder<impl Write>,
        name: &Path,
        control: &WorkspaceControl,
    ) -> Result<()> {
        let file = fs::File::open(&self.path).map_err(io)?;
        let mut reader = VerifiedReader {
            file,
            hash: Sha256::new(),
            control,
        };
        let mut header = tar::Header::new_gnu();
        header.set_metadata(&self.metadata);
        header.set_size(self.metadata.len());
        let streamed = archive
            .append_data(&mut header, name, &mut reader)
            .map_err(io);
        let changed = || {
            super::super::snapshot::snapshot_construction_failure(
                "content",
                &self.path,
                None,
                "source file changed while streaming; refusing a mixed snapshot",
            )
        };
        let after = fs::metadata(&self.path).map_err(|_| changed())?;
        let mut stable = self.metadata.len() == after.len()
            && self.metadata.modified().ok() == after.modified().ok();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            stable &= self.metadata.ino() == after.ino() && self.metadata.dev() == after.dev();
        }
        if !stable || format!("sha256:{:x}", reader.hash.finalize()) != self.digest {
            control.checkpoint()?;
            return Err(changed());
        }
        streamed?;
        control.checkpoint()
    }
}

struct VerifiedReader<'a> {
    file: fs::File,
    hash: Sha256,
    control: &'a WorkspaceControl,
}
impl Read for VerifiedReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.control
            .checkpoint()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let n = self.file.read(bytes)?;
        self.hash.update(&bytes[..n]);
        Ok(n)
    }
}

/// The producer writes directly to the consumer, with no archive or source
/// staging copy. Dropping either side breaks the pipe and joins its peer.
#[cfg(unix)]
pub(in crate::workspace) fn pipe<T: Send>(
    produce: impl FnOnce(fs::File) -> Result<()> + Send,
    consume: impl FnOnce(fs::File) -> Result<T>,
) -> Result<T> {
    use std::os::fd::{FromRawFd, IntoRawFd};
    let (reader, writer) = std::os::unix::net::UnixStream::pair().map_err(io)?;
    let reader = unsafe { fs::File::from_raw_fd(reader.into_raw_fd()) };
    let writer = unsafe { fs::File::from_raw_fd(writer.into_raw_fd()) };
    std::thread::scope(|scope| {
        let producer = scope.spawn(move || produce(writer));
        let mut result = consume(reader);
        let produced = producer
            .join()
            .map_err(|_| io("blob stream producer panicked"))?;
        if let Err(error) = produced {
            if error.details["classification"] == "snapshot_construction" || result.is_ok() {
                return Err(error);
            }
            // A rejected/truncated archive can make the consumer fail too.
            // Keep transport classification and commit uncertainty, but never
            // discard the controller-side cause (including missing files).
            if let Err(consumer) = &mut result {
                consumer.details["blob_stream_producer"] = serde_json::json!({
                    "code": error.code.as_str(),
                    "message": error.message,
                    "details": error.details,
                });
            }
        }
        result
    })
}
