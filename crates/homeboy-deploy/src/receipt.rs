use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use homeboy_core::error::Error;
use homeboy_core::project::Project;

use super::content_manifest::Manifest;
use super::types::{BuildProvenance, ContentManifestProvenance};

/// Classify a receipt IO failure, naming the operation that failed.
///
/// Routed through [`Error::from_io_error`] on purpose: that is where storage
/// exhaustion is classified. `.map_err(|error| error.to_string())` reported a
/// full disk as an ordinary sentence, which is precisely the loss #11135
/// describes — the receipt write is the last step of a successful deploy, so an
/// ENOSPC here needs to be distinguishable from a corrupt receipt.
fn receipt_io_error(error: &std::io::Error, operation: &str, path: &Path) -> Error {
    Error::from_io_error(error, Some(format!("{operation} '{}'", path.display())))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DeployedPackageReceipt {
    project_id: String,
    component_id: String,
    target: String,
    version: String,
    pub(super) manifest: Manifest,
    pub(super) payload_sha256: String,
    pub(super) build_provenance: BuildProvenance,
    package_scope: String,
    manifest_digest: String,
    pub(super) exclusions: Vec<String>,
}

impl DeployedPackageReceipt {
    pub(super) fn provenance(&self) -> ContentManifestProvenance {
        ContentManifestProvenance {
            source: "deployed-package-receipt".to_string(),
            artifact_name: self
                .build_provenance
                .artifact_identity
                .as_ref()
                .map(|identity| identity.path.clone()),
            artifact_sha256: Some(self.payload_sha256.clone()),
            artifact_tag: self.build_provenance.built_from_ref.clone(),
            artifact_commit: self.build_provenance.built_from_commit.clone(),
        }
    }
}

pub(super) fn load(
    project: &Project,
    component_id: &str,
    target: &str,
    version: &str,
    exclusions: &[String],
) -> Result<Option<DeployedPackageReceipt>, Error> {
    let path = path_in_roots(
        homeboy_paths::PathRoots::from_environment()?.data(),
        project,
        component_id,
        target,
        version,
    );
    if !path.exists() {
        return Ok(None);
    }
    let receipt: DeployedPackageReceipt =
        serde_json::from_slice(&read_no_follow(&path)?).map_err(|error| {
            Error::from_json_error(
                &error,
                Some(format!(
                    "parse deployed package receipt '{}'",
                    path.display()
                )),
            )
        })?;
    receipt.validate(project, component_id, target, version, exclusions, &path)?;
    Ok(Some(receipt))
}

pub(super) struct ReceiptWrite<'a> {
    pub(super) project: &'a Project,
    pub(super) component_id: &'a str,
    pub(super) target: &'a str,
    pub(super) version: &'a str,
    pub(super) manifest: Manifest,
    pub(super) payload_sha256: String,
    pub(super) build_provenance: BuildProvenance,
    pub(super) exclusions: Vec<String>,
}

pub(super) fn write(input: ReceiptWrite<'_>) -> Result<(), Error> {
    let ReceiptWrite {
        project,
        component_id,
        target,
        version,
        manifest,
        payload_sha256,
        build_provenance,
        exclusions,
    } = input;
    let path = path_in_roots(
        homeboy_paths::PathRoots::from_environment()?.data(),
        project,
        component_id,
        target,
        version,
    );
    let parent = path.parent().expect("receipt path has a parent");
    fs::create_dir_all(parent)
        .map_err(|error| receipt_io_error(&error, "create deploy receipt directory", parent))?;
    restrict_directory(parent)?;
    let receipt = DeployedPackageReceipt {
        project_id: project.id.clone(),
        component_id: component_id.to_string(),
        target: target.to_string(),
        version: version.to_string(),
        manifest_digest: manifest.digest(),
        manifest,
        payload_sha256,
        build_provenance,
        package_scope: "canonical-package-installed-tree".to_string(),
        exclusions,
    };
    let temporary = parent.join(format!(".receipt-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = create_private_file(&temporary)?;
    let bytes = serde_json::to_vec(&receipt).map_err(|error| {
        Error::from_json_error(
            &error,
            Some(format!(
                "serialize deployed package receipt '{}'",
                path.display()
            )),
        )
    })?;
    file.write_all(&bytes)
        .map_err(|error| receipt_io_error(&error, "write deploy receipt", &temporary))?;
    file.sync_all()
        .map_err(|error| receipt_io_error(&error, "sync deploy receipt", &temporary))?;
    fs::rename(&temporary, &path)
        .map_err(|error| receipt_io_error(&error, "publish deploy receipt", &path))?;
    sync_directory(parent)
}

impl DeployedPackageReceipt {
    fn validate(
        &self,
        project: &Project,
        component_id: &str,
        target: &str,
        version: &str,
        exclusions: &[String],
        path: &Path,
    ) -> Result<(), Error> {
        if self.project_id != project.id
            || self.component_id != component_id
            || self.target != target
            || self.version != version
            || self.package_scope != "canonical-package-installed-tree"
            || self.exclusions != exclusions
        {
            return Err(Error::invalid_argument_for(
                "deployed_package_receipt",
                "deployed-package receipt does not match the requested deployment identity",
                path.display().to_string(),
            )
            .with_hint(
                "The receipt was written for a different project, component, target, version, or \
                 exclusion set; redeploy to regenerate it.",
            ));
        }
        self.manifest.validate()?;
        if self.manifest.digest() != self.manifest_digest
            || self.payload_sha256.len() != 64
            || !self
                .payload_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::invalid_argument_for(
                "deployed_package_receipt",
                "deployed-package receipt manifest integrity validation failed",
                path.display().to_string(),
            )
            .with_hint(
                "The receipt's recorded manifest digest or payload digest does not describe its \
                 own contents; delete it and redeploy to regenerate it.",
            ));
        }
        Ok(())
    }
}

pub(super) fn invalidate(
    project: &Project,
    component_id: &str,
    target: &str,
    version: &str,
) -> Result<(), Error> {
    let path = path_in_roots(
        homeboy_paths::PathRoots::from_environment()?.data(),
        project,
        component_id,
        target,
        version,
    );
    match fs::remove_file(&path) {
        Ok(()) => sync_directory(path.parent().expect("receipt path has a parent")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(receipt_io_error(&error, "remove deploy receipt", &path)),
    }
}

fn create_private_file(path: &Path) -> Result<fs::File, Error> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|error| receipt_io_error(&error, "create deploy receipt temporary", path))
}

fn read_no_follow(path: &Path) -> Result<Vec<u8>, Error> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|error| receipt_io_error(&error, "open deploy receipt", path))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)
        .map_err(|error| receipt_io_error(&error, "read deploy receipt", path))?;
    Ok(bytes)
}

fn restrict_directory(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| receipt_io_error(&error, "restrict deploy receipt directory", path))?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| receipt_io_error(&error, "sync deploy receipt directory", path))?;
    }
    Ok(())
}

/// The receipt path for one deployment identity, below an explicitly injected
/// data root.
///
/// Infallible now that the root is supplied: resolution was the only fallible
/// step. The `homeboy_data` error it used to propagate is raised once by the
/// caller's `PathRoots::from_environment`, which reports the same coded error
/// rather than flattening a missing or unwritable data root into a sentence.
fn path_in_roots(
    data_root: &Path,
    project: &Project,
    component_id: &str,
    target: &str,
    version: &str,
) -> PathBuf {
    let mut hash = Sha256::new();
    for value in [&project.id, component_id, target, version] {
        hash.update(value.as_bytes());
        hash.update([0]);
    }
    data_root
        .join("deploy-receipts")
        .join(format!("{:x}.json", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BuildPhase, BuildSource};
    use homeboy_core::error::ErrorCode;

    fn build_provenance() -> BuildProvenance {
        BuildProvenance {
            source: BuildSource::DownloadedRelease,
            phase: BuildPhase::NotRun,
            build_ran: false,
            built_from_ref: None,
            built_from_commit: None,
            working_tree_dirty: None,
            artifact_identity: None,
        }
    }

    /// #11135: a receipt that fails its own integrity check must reach the
    /// caller as a coded error naming the receipt file, with a next action.
    #[test]
    fn receipt_identity_mismatch_is_coded_and_names_the_receipt() {
        let receipt = DeployedPackageReceipt {
            project_id: "other".to_string(),
            component_id: "component".to_string(),
            target: "/srv/site".to_string(),
            version: "1.0.0".to_string(),
            manifest: Manifest::default(),
            payload_sha256: "0".repeat(64),
            build_provenance: build_provenance(),
            package_scope: "canonical-package-installed-tree".to_string(),
            manifest_digest: Manifest::default().digest(),
            exclusions: Vec::new(),
        };
        let project = Project {
            id: "project".to_string(),
            ..Project::default()
        };

        let error = receipt
            .validate(
                &project,
                "component",
                "/srv/site",
                "1.0.0",
                &[],
                Path::new("/var/lib/homeboy/deploy-receipts/abc.json"),
            )
            .expect_err("identity mismatch");

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(
            error.details.get("id").and_then(|id| id.as_str()),
            Some("/var/lib/homeboy/deploy-receipts/abc.json")
        );
        assert!(!error.hints.is_empty());
    }

    /// Receipt IO keeps its classification: the operation is carried in
    /// `details.context`, and a full disk stays a storage-exhaustion failure
    /// instead of collapsing into an ordinary sentence.
    #[test]
    fn receipt_io_failures_are_classified_rather_than_stringified() {
        let error = receipt_io_error(
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            "write deploy receipt",
            Path::new("/var/lib/homeboy/deploy-receipts/abc.json"),
        );
        assert_eq!(error.code, ErrorCode::InternalIoError);
        assert!(
            error.details.to_string().contains("write deploy receipt"),
            "{}",
            error.details
        );

        let exhausted = receipt_io_error(
            &std::io::Error::other("No space left on device"),
            "write deploy receipt",
            Path::new("/var/lib/homeboy/deploy-receipts/abc.json"),
        );
        assert!(
            exhausted.is_storage_exhausted(),
            "a full disk must stay distinguishable from a corrupt receipt: {exhausted:?}"
        );
    }
}
