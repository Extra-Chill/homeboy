use super::*;
use std::io::Read;

const PUBLIC_ARTIFACT_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const MAX_PUBLIC_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

/// Resolve an artifact record by run/artifact token, validating that the
/// recorded `run_id` matches the requested run.
///
/// The previous CLI helper indexed nested publication artifact refs before
/// looking up the artifact; this helper preserves that order.
pub fn resolve_artifact_for_run(
    store: &ObservationStore,
    run_id: &str,
    artifact_id: &str,
) -> Result<ArtifactRecord> {
    let run = require_run(store, run_id)?;
    crate::artifacts::index_remote_published_artifact_refs_for_run(store, &run.id)?;
    let records = store.list_artifacts(&run.id)?;
    select_artifact_record(&run.id, artifact_id, &records, |id| {
        lookup_artifact_reference(&run.id, id)
    })?
    .ok_or_else(|| unknown_artifact_error(store, &run.id, artifact_id))
}

fn selection_error(token: &str, message: impl Into<String>) -> Error {
    Error::validation_invalid_argument("artifact_id", message.into(), Some(token.to_string()), None)
}

/// Resolve metadata through the registered control-plane owner, preserving its
/// failure instead of pretending a pointer identity was an unknown byte ID.
pub fn lookup_artifact_reference(
    run_id: &str,
    id: &str,
) -> Result<homeboy_control_plane_contract::ControlPlaneReference> {
    use homeboy_control_plane_contract::{ControlPlaneReferenceType, ReferenceId, RunId};
    let invalid = |err: String| selection_error(id, err);
    let run = RunId::new(run_id).map_err(|e| invalid(e.to_string()))?;
    let reference = ReferenceId::new(id).map_err(|e| invalid(e.to_string()))?;
    crate::control_plane::reference(&run, ControlPlaneReferenceType::Artifact, &reference)
        .map_err(|e| invalid(format!("artifact reference lookup failed: {e}")))
}

/// Shared local/connected-runner metadata selection, before either byte reader.
/// Callers must supply a complete run inventory. Canonical pins always win;
/// logical selectors and friendly aliases must prove a unique record.
pub fn select_artifact_record(
    run_id: &str,
    token: &str,
    records: &[ArtifactRecord],
    lookup_reference: impl FnOnce(&str) -> Result<homeboy_control_plane_contract::ControlPlaneReference>,
) -> Result<Option<ArtifactRecord>> {
    let scoped: Vec<_> = records
        .iter()
        .filter(|record| record.run_id == run_id)
        .collect();
    if let Some(record) = scoped.iter().find(|record| record.id == token) {
        return Ok(Some((*record).clone()));
    }
    let typed_reference = if token.starts_with("artifact/") {
        Some(
            token
                .parse::<homeboy_control_plane_contract::ControlPlaneRef>()
                .map_err(|error| selection_error(token, error.to_string()))?,
        )
    } else {
        None
    };
    let reference_id = typed_reference
        .as_ref()
        .map(|reference| reference.identity_str())
        .or_else(|| {
            token
                .strip_prefix("artifact-")
                .filter(|suffix| {
                    suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .map(|_| token)
        });
    // Bare tokens retain their established friendly-name meaning when the
    // inventory records one. Shape alone cannot authorize a pointer. A caller
    // can disambiguate a colliding pointer with the typed artifact/<id> form.
    let reference_id = reference_id.filter(|_| {
        typed_reference.is_some()
            || !scoped
                .iter()
                .any(|record| artifact_matches_friendly_token(record, token))
    });
    let reference = reference_id.map(lookup_reference).transpose()?;
    if let Some(reference) = &reference {
        if reference.run.as_str() != run_id
            || reference.reference_type
                != homeboy_control_plane_contract::ControlPlaneReferenceType::Artifact
            || Some(reference.reference.as_str()) != reference_id
        {
            return Err(selection_error(
                token,
                "artifact reference does not belong to requested run/reference",
            ));
        }
    }
    let uri = reference
        .as_ref()
        .map(|reference| reference.uri.as_str())
        .unwrap_or(token);
    let qualified = reference.is_some() || uri.starts_with("homeboy://");
    let candidates: Vec<_> = if qualified {
        let selector = crate::artifact_ref::AgentTaskArtifactSelector::parse(uri)
            .ok_or_else(|| selection_error(token, "artifact reference has no supported producer-qualified byte selector; inspect the reference and use a canonical artifact ID from `homeboy runs artifacts <run-id> --full`"))?;
        if selector.run_id != run_id {
            return Err(selection_error(
                token,
                "artifact selector does not belong to requested run",
            ));
        }
        scoped
            .into_iter()
            .filter(|record| {
                let metadata = &record.metadata_json["agent_task"];
                metadata["task_id"].as_str() == Some(selector.task_id.as_str())
                    && metadata["logical_artifact_id"].as_str()
                        == Some(selector.logical_artifact_id.as_str())
            })
            .collect()
    } else {
        scoped
            .into_iter()
            .filter(|record| artifact_matches_friendly_token(record, token))
            .collect()
    };
    match candidates.as_slice() {
        [record] => Ok(Some((*record).clone())),
        [] if qualified => Err(selection_error(token, "artifact reference has no persisted byte record for the selected run/task/logical artifact; retrieval remains unproven")),
        [] => Ok(None),
        _ => {
            let mut ids: Vec<_> = candidates.iter().map(|record| record.id.as_str()).collect();
            ids.sort_unstable();
            Err(selection_error(token, format!("artifact selection is ambiguous; pin a canonical artifact ID: {}", ids.join(", "))))
        }
    }
}

fn artifact_matches_friendly_token(record: &ArtifactRecord, token: &str) -> bool {
    record.kind == token
        || record.metadata_json["name"].as_str() == Some(token)
        || record.metadata_json["original_manifest_id"].as_str() == Some(token)
}

#[cfg(test)]
mod reference_byte_selection_tests {
    use super::*;
    use homeboy_control_plane_contract::{
        ControlPlaneReference, ControlPlaneReferenceType, ReferenceId, RunId,
    };
    const STATUS_ID: &str = "artifact-0123456789abcdef0123456789abcdef";

    fn record(id: &str, task: &str) -> ArtifactRecord {
        ArtifactRecord {
            id: id.to_string(),
            run_id: "run-a".to_string(),
            kind: "patch".to_string(),
            artifact_type: "file".to_string(),
            path: "/unavailable/retained.patch".to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: vec![],
            sha256: Some("retained-content-identity".to_string()),
            size_bytes: Some(11),
            mime: None,
            metadata_json: serde_json::json!({"agent_task": {"task_id": task, "logical_artifact_id": "patch"}}),
            created_at: "2026-10-08T00:00:00Z".to_string(),
        }
    }

    fn reference(id: &str, uri: &str) -> ControlPlaneReference {
        ControlPlaneReference {
            schema: homeboy_control_plane_contract::CONTROL_PLANE_REFERENCE_SCHEMA.to_string(),
            run: RunId::new("run-a").unwrap(),
            reference_type: ControlPlaneReferenceType::Artifact,
            reference: ReferenceId::new(id).unwrap(),
            kind: "patch".to_string(),
            uri: uri.to_string(),
            registered_at: None,
            actor: None,
        }
    }

    #[test]
    fn reference_byte_selection_requires_unique_run_and_producer_metadata() {
        let uri = "homeboy://agent-task/run/run-a/artifacts#task=first&artifact=patch";
        let first = record("controller-retained-id", "first");
        let second = record("other-producer-id", "second");
        let records = vec![second, first.clone()];
        for token in [STATUS_ID, "artifact/artifact-pointer", uri] {
            let selected =
                select_artifact_record("run-a", token, &records, |id| Ok(reference(id, uri)))
                    .unwrap()
                    .unwrap();
            assert_eq!(selected.id, first.id);
        }
        let mut wrong_run = first.clone();
        wrong_run.run_id = "run-b".to_string();
        assert!(
            select_artifact_record("run-a", uri, &[wrong_run], |_| unreachable!())
                .unwrap_err()
                .to_string()
                .contains("no persisted")
        );
        for invalid in [
            "homeboy://agent-task/run/run-b/artifacts#task=first&artifact=patch",
            "homeboy://agent-task/run/run-a/artifacts#task=missing&artifact=patch",
            "homeboy://agent-task/run/run-a/artifacts#task=first&task=second&artifact=patch",
            "homeboy://agent-task/run/run-a/artifacts#task=first&task&artifact=patch",
            "homeboy://agent-task/run/run-a/artifacts#task=%GG&artifact=patch",
            "homeboy://agent-task/run/run-a/artifacts#task=first&artifact=",
        ] {
            assert!(
                select_artifact_record("run-a", invalid, &records, |_| unreachable!()).is_err(),
                "{invalid}"
            );
        }
        assert!(select_artifact_record("run-a", STATUS_ID, &records, |id| {
            let mut resource = reference(id, uri);
            resource.run = RunId::new("run-b").unwrap();
            Ok(resource)
        })
        .is_err());
        assert!(
            select_artifact_record("run-a", STATUS_ID, &[], |id| Ok(reference(id, uri)))
                .unwrap_err()
                .to_string()
                .contains("no persisted")
        );
        let unsupported = select_artifact_record("run-a", STATUS_ID, &records, |id| {
            Ok(reference(id, "file:///producer/patch"))
        })
        .unwrap_err();
        assert!(unsupported.to_string().contains("canonical artifact ID"));
        for raw in ["file:///producer/patch", "/producer/patch"] {
            assert!(
                select_artifact_record("run-a", raw, &records, |_| unreachable!())
                    .unwrap()
                    .is_none()
            );
        }
        let mut friendly = first;
        friendly.kind = "artifact-report".to_string();
        assert_eq!(
            select_artifact_record("run-a", "artifact-report", &[friendly], |_| unreachable!())
                .unwrap()
                .unwrap()
                .id,
            "controller-retained-id"
        );
    }

    #[test]
    fn reference_byte_selection_reports_all_candidates_and_keeps_exact_pins() {
        let uri = "homeboy://agent-task/run/run-a/artifacts#task=first&artifact=patch";
        let first = record("retained-a", "first");
        let mut copy = first.clone();
        copy.id = "retained-b".to_string();
        for digest in [
            first.sha256.clone(),
            Some("conflicting-content".to_string()),
        ] {
            copy.sha256 = digest;
            let records = vec![copy.clone(), first.clone()];
            let error = select_artifact_record("run-a", uri, &records, |_| unreachable!())
                .unwrap_err()
                .to_string();
            assert!(error.contains("ambiguous") && error.contains("retained-a, retained-b"));
            assert_eq!(
                select_artifact_record("run-a", "retained-a", &records, |_| unreachable!())
                    .unwrap()
                    .unwrap()
                    .id,
                "retained-a"
            );
            assert!(
                select_artifact_record("run-a", "patch", &records, |_| unreachable!())
                    .unwrap_err()
                    .to_string()
                    .contains("ambiguous")
            );
        }
        copy.id = "metadata-only-id".to_string();
        copy.artifact_type = "metadata-only".to_string();
        let selected = select_artifact_record("run-a", uri, &[copy], |_| unreachable!())
            .unwrap()
            .unwrap();
        assert_eq!(
            classify_artifact_storage(&selected),
            ArtifactStorage::MetadataOnly
        );
        assert!(copy_local_file_artifact(selected, None)
            .unwrap_err()
            .to_string()
            .contains("not a downloadable file"));
        assert!(copy_local_file_artifact(first, None)
            .unwrap_err()
            .to_string()
            .contains("missing or unreadable"));
    }

    #[test]
    fn reference_byte_selection_does_not_infer_pointer_authority_from_friendly_token_shape() {
        let uri = "homeboy://agent-task/run/run-a/artifacts#task=first&artifact=patch";
        let target = record("retained-target", "first");
        for field in ["kind", "name", "original_manifest_id"] {
            let mut friendly = record("retained-friendly", "other");
            if field == "kind" {
                friendly.kind = STATUS_ID.to_string();
            } else {
                friendly.metadata_json[field] = serde_json::json!(STATUS_ID);
            }
            let records = vec![target.clone(), friendly];
            assert_eq!(
                select_artifact_record("run-a", STATUS_ID, &records, |_| panic!(
                    "friendly token is not a pointer"
                ))
                .unwrap()
                .unwrap()
                .id,
                "retained-friendly"
            );
            let typed = format!("artifact/{STATUS_ID}");
            assert_eq!(
                select_artifact_record("run-a", &typed, &records, |id| Ok(reference(id, uri)))
                    .unwrap()
                    .unwrap()
                    .id,
                "retained-target"
            );
        }
        assert!(
            select_artifact_record("run-a", STATUS_ID, &[target.clone()], |_| Err(
                selection_error(STATUS_ID, "reference lookup unavailable")
            ))
            .unwrap_err()
            .to_string()
            .contains("lookup unavailable")
        );
        let mut pin = record(STATUS_ID, "other");
        pin.kind = STATUS_ID.to_string();
        assert_eq!(
            select_artifact_record("run-a", STATUS_ID, &[pin, target], |_| panic!(
                "exact canonical pin needs no pointer lookup"
            ))
            .unwrap()
            .unwrap()
            .id,
            STATUS_ID
        );
    }
}

/// Build a clear "artifact not found" error that lists the artifact names
/// (kinds and ids) actually recorded for the run, so callers fix the token
/// instead of guessing which name matches.
fn unknown_artifact_error(store: &ObservationStore, run_id: &str, artifact_id: &str) -> Error {
    let available = store.list_artifacts(run_id).unwrap_or_default();
    let mut names: Vec<String> = Vec::new();
    for artifact in &available {
        for token in [artifact.kind.as_str(), artifact.id.as_str()] {
            if !token.is_empty() && !names.iter().any(|name| name == token) {
                names.push(token.to_string());
            }
        }
    }
    let (problem, hints) = if names.is_empty() {
        (
            format!("artifact record not found: {artifact_id}; run `{run_id}` has no recorded artifacts yet"),
            vec!["Run `homeboy runs artifacts <run-id>` after the source command records artifacts.".to_string()],
        )
    } else {
        (
            format!(
                "artifact record not found: {artifact_id}; available artifact names for run `{run_id}`: {}",
                names.join(", ")
            ),
            vec![
                "Pass an artifact id, kind, or name from the available list.".to_string(),
                "Run `homeboy runs artifacts <run-id>` to inspect all recorded artifacts."
                    .to_string(),
            ],
        )
    };
    Error::validation_invalid_argument(
        "artifact_id",
        problem,
        Some(artifact_id.to_string()),
        Some(hints),
    )
}

/// Copy a recorded file artifact's bytes to `output`.
///
/// Returns a stable `ArtifactFetchOutcome` so callers can present the
/// summary in their preferred format. Validates that the artifact is a
/// local file (callers should detect remote/metadata-only artifacts and
/// dispatch separately).
pub fn copy_local_file_artifact(
    artifact: ArtifactRecord,
    output: Option<PathBuf>,
) -> Result<ArtifactFetchOutcome> {
    if artifact.artifact_type != "file" {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} is {}, not a downloadable file",
                artifact.id, artifact.artifact_type
            ),
            Some(artifact.id),
            None,
        ));
    }

    let source = PathBuf::from(&artifact.path);
    if !source.is_file() {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} file is missing or unreadable at {}; rerun the source command or import a bundle that includes artifact bytes",
                artifact.id,
                source.display()
            ),
            Some(artifact.id),
            None,
        ));
    }
    let file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&artifact.id)
        .to_string();
    let output = output.unwrap_or_else(|| PathBuf::from(file_name));
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }

    let mut reader = File::open(&source).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("open artifact {}", source.display())),
        )
    })?;
    let mut writer = File::create(&output).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("create {}", output.display())))
    })?;
    io::copy(&mut reader, &mut writer).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "copy artifact {} to {}",
                artifact.id,
                output.display()
            )),
        )
    })?;
    // Flush and fsync so the reported `output_path` is durably on disk
    // before we return success — never print fetch metadata for a write
    // that did not actually land.
    writer.flush().map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("flush {}", output.display())))
    })?;
    writer.sync_all().map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("sync {}", output.display())))
    })?;
    drop(writer);
    if !output.is_file() {
        return Err(Error::internal_io(
            format!(
                "artifact {} copy reported success but no file exists at {}",
                artifact.id,
                output.display()
            ),
            Some(format!("verify artifact output {}", output.display())),
        ));
    }

    Ok(ArtifactFetchOutcome {
        run_id: artifact.run_id,
        artifact_id: artifact.id,
        output_path: output,
        content_type: artifact.mime,
        size_bytes: artifact.size_bytes,
        sha256: artifact.sha256,
        artifact_ref: None,
    })
}

/// Download a remote runner artifact and report the same normalized fetch
/// outcome used by local artifact copies.
pub fn download_remote_artifact(
    artifact: ArtifactRecord,
    output: Option<PathBuf>,
) -> Result<ArtifactFetchOutcome> {
    let download = runner_evidence::with_runner_evidence(|p| {
        p.download_remote_artifact(&artifact.path, output)
    })?;
    Ok(ArtifactFetchOutcome {
        run_id: artifact.run_id,
        artifact_id: artifact.id,
        output_path: download.output_path,
        content_type: download.content_type,
        size_bytes: download.size_bytes,
        sha256: download.sha256,
        artifact_ref: Some(download.artifact_ref),
    })
}

/// Fetch a declared public artifact without using runner credentials. Public
/// locators survive ephemeral runner cleanup; the timeout and streaming cap
/// keep an unreachable or oversized object from stalling `artifact get`.
pub fn download_public_artifact(
    artifact: ArtifactRecord,
    output: Option<PathBuf>,
) -> Result<ArtifactFetchOutcome> {
    let url = artifact
        .url
        .as_deref()
        .or(artifact.public_url.as_deref())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                format!("artifact {} has no public content URL", artifact.id),
                Some(artifact.id.clone()),
                None,
            )
        })?;
    let parsed = reqwest::Url::parse(url).map_err(|err| {
        Error::validation_invalid_argument(
            "artifact_id",
            err.to_string(),
            Some(artifact.id.clone()),
            None,
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "public artifact URL must use HTTP or HTTPS",
            Some(artifact.id.clone()),
            None,
        ));
    }
    let client =
        crate::http_probe::blocking_client(PUBLIC_ARTIFACT_FETCH_TIMEOUT).map_err(|err| {
            Error::internal_unexpected(format!("build public artifact client: {err}"))
        })?;
    let mut response = client.get(parsed.clone()).send().map_err(|err| {
        Error::internal_unexpected(format!("fetch public artifact {}: {err}", artifact.id))
    })?;
    if !response.status().is_success() {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "public artifact {} returned HTTP {}",
                artifact.id,
                response.status()
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_PUBLIC_ARTIFACT_BYTES)
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "public artifact {} exceeds the {} byte download limit",
                artifact.id, MAX_PUBLIC_ARTIFACT_BYTES
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    let filename = parsed
        .path_segments()
        .and_then(Iterator::last)
        .filter(|name| !name.is_empty())
        .unwrap_or(&artifact.id)
        .to_string();
    let output = output.unwrap_or_else(|| PathBuf::from(filename));
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let mut writer = File::create(&output).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("create {}", output.display())),
        )
    })?;
    let copied = io::copy(
        &mut response.by_ref().take(MAX_PUBLIC_ARTIFACT_BYTES + 1),
        &mut writer,
    )
    .map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("download public artifact {}", artifact.id)),
        )
    })?;
    if copied > MAX_PUBLIC_ARTIFACT_BYTES {
        drop(writer);
        std::fs::remove_file(&output).ok();
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "public artifact {} exceeds the {} byte download limit",
                artifact.id, MAX_PUBLIC_ARTIFACT_BYTES
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    writer.flush().map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("flush {}", output.display())))
    })?;
    writer.sync_all().map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("sync {}", output.display())))
    })?;
    Ok(ArtifactFetchOutcome {
        run_id: artifact.run_id,
        artifact_id: artifact.id,
        output_path: output,
        content_type: response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .or(artifact.mime),
        size_bytes: i64::try_from(copied).ok(),
        sha256: artifact.sha256,
        artifact_ref: None,
    })
}

/// Classify an artifact's storage so callers can decide between local
/// copy, remote download, or a metadata-only error.
pub fn classify_artifact_storage(artifact: &ArtifactRecord) -> ArtifactStorage {
    if artifact.artifact_type == "file" {
        return ArtifactStorage::LocalFile;
    }
    if artifact.artifact_type == "url"
        && artifact
            .url
            .as_deref()
            .or(artifact.public_url.as_deref())
            .is_some()
    {
        return ArtifactStorage::PublicUrl;
    }
    if crate::execution_contract::is_remote_runner_artifact_path(&artifact.path)
        || artifact.artifact_type == "remote_file"
    {
        return ArtifactStorage::Remote;
    }
    if artifact.artifact_type == "metadata-only" {
        return ArtifactStorage::MetadataOnly;
    }
    ArtifactStorage::Other
}

/// Hydrate remote runner artifacts into local-file artifact records by
/// downloading their bytes, so JSON summarizers (e.g. matrix-artifacts)
/// can parse remote finding-packets / result packets instead of seeing
/// an opaque `remote_file` they cannot read.
///
/// `should_hydrate` selects which remote artifacts are worth a round-trip
/// (so we never pull unrelated binaries). `download` performs the actual
/// per-artifact runner round-trip — that is the live hop and is supplied
/// by [`hydrate_remote_artifacts_via_runner`] in production. Everything
/// around it (selection, record rewriting, diagnostics) is pure and unit
/// tested with a fake downloader.
///
/// Failures never abort the pass: the original (un-hydrated) record is kept
/// and a diagnostic is recorded so the operator sees exactly which packet
/// was unreachable and why.
pub fn hydrate_remote_artifacts<P, F>(
    artifacts: Vec<ArtifactRecord>,
    mut should_hydrate: P,
    mut download: F,
) -> (Vec<ArtifactRecord>, Vec<String>)
where
    P: FnMut(&ArtifactRecord) -> bool,
    F: FnMut(&ArtifactRecord) -> Result<ArtifactFetchOutcome>,
{
    let mut hydrated = Vec::with_capacity(artifacts.len());
    let mut diagnostics = Vec::new();
    for artifact in artifacts {
        let is_remote = classify_artifact_storage(&artifact) == ArtifactStorage::Remote;
        if !is_remote || !should_hydrate(&artifact) {
            hydrated.push(artifact);
            continue;
        }
        match download(&artifact) {
            Ok(outcome) => {
                let mut record = artifact;
                record.artifact_type = "file".to_string();
                record.path = outcome.output_path.display().to_string();
                if record.mime.is_none() {
                    record.mime = outcome.content_type;
                }
                if record.size_bytes.is_none() {
                    record.size_bytes = outcome.size_bytes;
                }
                if record.sha256.is_none() {
                    record.sha256 = outcome.sha256;
                }
                hydrated.push(record);
            }
            Err(err) => {
                diagnostics.push(format!(
                    "remote artifact {} could not be hydrated for summary: {}",
                    artifact.id, err.message
                ));
                hydrated.push(artifact);
            }
        }
    }
    (hydrated, diagnostics)
}

/// Hydrate remote artifacts using the live runner download path
/// (`download_remote_artifact`, the same mechanism `runs artifacts --pull`
/// uses). Convenience wrapper over [`hydrate_remote_artifacts`].
pub fn hydrate_remote_artifacts_via_runner<P>(
    artifacts: Vec<ArtifactRecord>,
    should_hydrate: P,
) -> (Vec<ArtifactRecord>, Vec<String>)
where
    P: FnMut(&ArtifactRecord) -> bool,
{
    hydrate_remote_artifacts(artifacts, should_hydrate, |artifact| {
        download_remote_artifact(artifact.clone(), None)
    })
}

/// Resolve a run id *or* a human run label to an observation run id.
///
pub fn resolve_run_id_or_label(store: &ObservationStore, run_id_or_label: &str) -> Result<String> {
    require_run(store, run_id_or_label).map(|run| run.id)
}

/// Find the first run whose human label matches `label`. A run matches when
/// its id equals the label, its command carries `--run-id <label>`, or a
/// known metadata pointer (lab label / proof provenance) equals the label.
pub fn match_run_label(runs: &[RunRecord], label: &str) -> Option<RunRecord> {
    runs.iter()
        .find(|run| super::run_lookup::run_matches_label(run, label))
        .cloned()
}
