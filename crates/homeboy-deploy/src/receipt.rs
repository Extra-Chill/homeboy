use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use homeboy_core::project::Project;

use super::content_manifest::Manifest;
use super::types::{BuildProvenance, ContentManifestProvenance};

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
) -> Result<Option<DeployedPackageReceipt>, String> {
    let path = path(project, component_id, target, version)?;
    if !path.exists() {
        return Ok(None);
    }
    let receipt: DeployedPackageReceipt =
        serde_json::from_slice(&read_no_follow(&path)?).map_err(|error| {
            format!(
                "invalid deployed package receipt '{}': {error}",
                path.display()
            )
        })?;
    receipt.validate(project, component_id, target, version, exclusions)?;
    Ok(Some(receipt))
}

pub(super) fn write(
    project: &Project,
    component_id: &str,
    target: &str,
    version: &str,
    manifest: Manifest,
    payload_sha256: String,
    build_provenance: BuildProvenance,
    exclusions: Vec<String>,
) -> Result<(), String> {
    let path = path(project, component_id, target, version)?;
    let parent = path.parent().expect("receipt path has a parent");
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
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
    file.write_all(&serde_json::to_vec(&receipt).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, &path).map_err(|error| error.to_string())?;
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
    ) -> Result<(), String> {
        if self.project_id != project.id
            || self.component_id != component_id
            || self.target != target
            || self.version != version
            || self.package_scope != "canonical-package-installed-tree"
            || self.exclusions != exclusions
        {
            return Err(
                "deployed-package receipt does not match the requested deployment identity"
                    .to_string(),
            );
        }
        self.manifest.validate()?;
        if self.manifest.digest() != self.manifest_digest
            || self.payload_sha256.len() != 64
            || !self
                .payload_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(
                "deployed-package receipt manifest integrity validation failed".to_string(),
            );
        }
        Ok(())
    }
}

pub(super) fn invalidate(
    project: &Project,
    component_id: &str,
    target: &str,
    version: &str,
) -> Result<(), String> {
    let path = path(project, component_id, target, version)?;
    match fs::remove_file(&path) {
        Ok(()) => sync_directory(path.parent().expect("receipt path has a parent")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn create_private_file(path: &Path) -> Result<fs::File, String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|error| error.to_string())
}

fn read_no_follow(path: &Path) -> Result<Vec<u8>, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn restrict_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn path(
    project: &Project,
    component_id: &str,
    target: &str,
    version: &str,
) -> Result<PathBuf, String> {
    let mut hash = Sha256::new();
    for value in [&project.id, component_id, target, version] {
        hash.update(value.as_bytes());
        hash.update([0]);
    }
    Ok(homeboy_paths::homeboy_data()
        .map_err(|error| error.to_string())?
        .join("deploy-receipts")
        .join(format!("{:x}.json", hash.finalize())))
}
