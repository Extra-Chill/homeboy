use crate::core::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("open artifact bytes {}", path.display())),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read artifact bytes {}", path.display())),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn content_type_from_path(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let mime = match extension.as_str() {
        "json" => "application/json",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => return None,
    };
    Some(mime.to_string())
}
