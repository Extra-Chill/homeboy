use crate::error::Result;
use std::path::Path;

/// SHA-256 of a file's bytes as lowercase hex.
///
/// Retained as the historical entry point for artifact hashing; the streaming
/// implementation lives in `homeboy_engine_primitives::content_hash` so every
/// crate produces byte-identical artifact identities.
pub fn sha256_file(path: &Path) -> Result<String> {
    homeboy_engine_primitives::content_hash::sha256_file(path)
}

pub fn content_type_from_path(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let mime = match extension.as_str() {
        "json" => "application/json",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "txt" | "log" => "text/plain",
        "patch" => "text/x-patch",
        "diff" => "text/x-diff",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => return None,
    };
    Some(mime.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executor_artifact_extensions_have_stable_content_types() {
        for (path, expected) in [
            ("change.patch", "text/x-patch"),
            ("change.diff", "text/x-diff"),
            ("transcript.txt", "text/plain"),
            ("result.json", "application/json"),
            ("runtime.log", "text/plain"),
        ] {
            assert_eq!(
                content_type_from_path(Path::new(path)).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(content_type_from_path(Path::new("artifact.unknown")), None);
    }
}
