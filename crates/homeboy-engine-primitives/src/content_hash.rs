//! Canonical SHA-256 content hashing.
//!
//! Content hashing is an *identity* primitive: artifact addressing, patch
//! verification, binary provenance, and broker token hashing all compare
//! digests produced in different crates. Every one of those comparisons is
//! only sound if the bytes-to-string mapping is byte-identical everywhere, so
//! there is exactly one implementation and it lives here.
//!
//! Output contract:
//! - Lowercase hex, never uppercase.
//! - Full 64 characters, never truncated.
//! - `sha256_file` streams the file in 64 KiB chunks so hashing a multi-gigabyte
//!   release artifact does not read it into memory.
//!
//! Deliberately *not* covered here: digests that hash something other than the
//! raw bytes (canonicalized overlays, ordered directory trees, salted inputs)
//! and digests that are intentionally truncated. Those are separate identity
//! schemes and collapsing them into this primitive would silently change what
//! they identify.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use homeboy_error::{Error, Result};

/// Chunk size used when streaming a file into the hasher.
const FILE_CHUNK_BYTES: usize = 64 * 1024;

/// SHA-256 of `bytes`, rendered as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// SHA-256 of the file at `path`, rendered as lowercase hex.
///
/// The file is streamed in [`FILE_CHUNK_BYTES`] chunks rather than read whole
/// into memory, so this is safe for arbitrarily large artifacts.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("open {} for sha256", path.display())),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; FILE_CHUNK_BYTES];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read {} for sha256", path.display())),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Whether `value` is shaped like a SHA-256 digest: exactly 64 hex characters.
///
/// Uppercase hex is **accepted**. This primitive validates values that arrive
/// from external producers (`git`, `sha256sum`, GitHub release metadata), which
/// are not all guaranteed to emit lowercase, and it preserves the behavior of
/// the hand-rolled validators it replaces. Values produced by [`sha256_hex`]
/// and [`sha256_file`] are always lowercase; use this only for validation, never
/// as a substitute for comparing canonical digests.
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// FIPS 180-2 known-answer vector for "abc".
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    /// SHA-256 of the empty input.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn sha256_hex_matches_known_answer_vector() {
        assert_eq!(sha256_hex(b"abc"), ABC_SHA256);
    }

    #[test]
    fn sha256_hex_of_empty_input_is_the_empty_digest() {
        assert_eq!(sha256_hex(b""), EMPTY_SHA256);
    }

    #[test]
    fn sha256_hex_output_is_lowercase_and_full_length() {
        let digest = sha256_hex(b"identity primitive");
        assert_eq!(digest.len(), 64);
        assert_eq!(digest, digest.to_ascii_lowercase());
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn sha256_file_matches_known_answer_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(sha256_file(&path).unwrap(), ABC_SHA256);
    }

    #[test]
    fn sha256_file_agrees_with_sha256_hex_across_chunk_boundaries() {
        // Larger than one read chunk so the streaming loop runs more than once
        // and a partial final chunk is exercised.
        let bytes: Vec<u8> = (0..(FILE_CHUNK_BYTES * 2 + 517))
            .map(|index| (index % 251) as u8)
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.bin");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        drop(file);

        assert_eq!(sha256_file(&path).unwrap(), sha256_hex(&bytes));
    }

    #[test]
    fn sha256_file_hashes_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(sha256_file(&path).unwrap(), EMPTY_SHA256);
    }

    #[test]
    fn sha256_file_errors_when_the_path_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(sha256_file(&dir.path().join("absent.bin")).is_err());
    }

    #[test]
    fn is_sha256_hex_accepts_canonical_lowercase_digests() {
        assert!(is_sha256_hex(ABC_SHA256));
        assert!(is_sha256_hex(&sha256_hex(b"anything")));
    }

    #[test]
    fn is_sha256_hex_accepts_uppercase_hex() {
        assert!(is_sha256_hex(&ABC_SHA256.to_ascii_uppercase()));
    }

    #[test]
    fn is_sha256_hex_rejects_wrong_length() {
        assert!(!is_sha256_hex(""));
        assert!(!is_sha256_hex(&ABC_SHA256[..63]));
        assert!(!is_sha256_hex(&format!("{ABC_SHA256}0")));
        // A 40-char git object id is not a sha256 digest.
        assert!(!is_sha256_hex("da39a3ee5e6b4b0d3255bfef95601890afd80709"));
    }

    #[test]
    fn is_sha256_hex_rejects_non_hex_characters() {
        let mut value = ABC_SHA256.to_string();
        value.replace_range(0..1, "z");
        assert!(!is_sha256_hex(&value));
        // Correct length, but a prefixed algorithm label is not bare hex.
        assert!(!is_sha256_hex(&format!("sha256:{}", &ABC_SHA256[7..])));
    }
}
