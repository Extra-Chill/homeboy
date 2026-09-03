//! Typed Extension API v1 registration inventory for agent-task executors.
//!
//! Executor declarations live on agent-runtime manifests, which core already
//! discovers. What core deliberately does not own is the resolved provider type:
//! that belongs to the agent-task layer. This module sits exactly on that line.
//! It answers "which executor identities does an installed, compatible extension
//! register, and is each one selectable", and it answers it from one immutable
//! snapshot so a caller cannot observe two different inventories inside a single
//! decision.
//!
//! Execution inputs never cross this boundary. Argv, commands, extension and
//! runtime paths, secret and environment declarations, materialization
//! contracts, and provider-specific options stay inside the manifest and the
//! agent-task layer.

use std::collections::BTreeMap;

use homeboy_extension_contract::agent_task_executor_declaration::{
    parse_agent_task_executor_declaration, AgentTaskExecutorProviderDeclaration,
};
use homeboy_extension_contract::api::v1::{
    ExtensionApiAgentTaskExecutorDescriptor, ExtensionApiAgentTaskExecutorDiagnostic,
    ExtensionApiAgentTaskExecutorDiagnosticKind, ExtensionApiAgentTaskExecutorInventoryRequest,
    ExtensionApiAgentTaskExecutorInventoryResponse, ExtensionApiAgentTaskExecutorValidation,
    ExtensionApiCatalogEntryStatus, ExtensionApiCatalogRequest, ExtensionApiOperationFailure,
    AGENT_TASK_EXECUTOR_CAPABILITY_PREFIX,
    EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_REQUEST_SCHEMA,
    EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_RESPONSE_SCHEMA,
    EXTENSION_API_CATALOG_REQUEST_SCHEMA, EXTENSION_API_V1,
};

use crate::extension::catalog::{snapshot_api, validate_operation_request};

/// One immutable registration inventory of every declared agent-task executor.
///
/// Construction is the only point at which extension state is read. Planning and
/// validation then share this snapshot, so an extension installed or removed
/// mid-decision cannot change an answer that has already been acted on.
pub struct AgentTaskExecutorApi {
    executors: Vec<ExtensionApiAgentTaskExecutorDescriptor>,
    failure: Option<ExtensionApiOperationFailure>,
}

impl AgentTaskExecutorApi {
    pub fn discover(request: &ExtensionApiAgentTaskExecutorInventoryRequest) -> Self {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return Self::failed(failure);
        }

        let snapshot = snapshot_api(&ExtensionApiCatalogRequest {
            schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
            api_version: request.api_version,
        });
        if let Some(failure) = snapshot.response.failure {
            return Self::failed(failure);
        }

        let mut executors = Vec::new();
        for entry in snapshot.response.entries {
            let Some(manifest) = snapshot.manifests.get(&entry.id) else {
                continue;
            };
            // An extension that is present but not usable still registers its
            // identities, so a caller reporting a missing executor can say why
            // rather than reporting it as simply absent.
            let unusable = match entry.status {
                ExtensionApiCatalogEntryStatus::Available => None,
                ExtensionApiCatalogEntryStatus::Incompatible => Some((
                    ExtensionApiAgentTaskExecutorDiagnosticKind::ExtensionIncompatible,
                    format!(
                        "Extension '{}' is not compatible with this Homeboy.",
                        entry.id
                    ),
                )),
                ExtensionApiCatalogEntryStatus::Invalid => Some((
                    ExtensionApiAgentTaskExecutorDiagnosticKind::ExtensionInvalid,
                    entry
                        .diagnostic
                        .as_ref()
                        .map(|diagnostic| diagnostic.message.clone())
                        .unwrap_or_else(|| format!("Extension '{}' is not usable.", entry.id)),
                )),
            };
            let advertised = entry
                .descriptor
                .as_ref()
                .into_iter()
                .flat_map(|descriptor| &descriptor.capabilities)
                .map(|capability| capability.id.as_str())
                .filter(|id| id.starts_with(AGENT_TASK_EXECUTOR_CAPABILITY_PREFIX))
                .map(str::to_string)
                .collect::<Vec<_>>();

            for runtime in &manifest.agent_runtimes {
                for declared in &runtime.agent_task_executors {
                    let declaration =
                        parse_agent_task_executor_declaration(&manifest.id, &runtime.id, declared);
                    executors.push(descriptor(
                        &manifest.id,
                        &runtime.id,
                        declaration,
                        &advertised,
                        unusable.clone(),
                    ));
                }
            }
        }

        mark_duplicate_ids(&mut executors);
        executors.sort_by(|left, right| {
            (&left.owning_extension, &left.runtime_id, &left.id).cmp(&(
                &right.owning_extension,
                &right.runtime_id,
                &right.id,
            ))
        });

        Self {
            executors,
            failure: None,
        }
    }

    fn failed(failure: ExtensionApiOperationFailure) -> Self {
        Self {
            executors: Vec::new(),
            failure: Some(failure),
        }
    }

    pub fn inventory_api(&self) -> ExtensionApiAgentTaskExecutorInventoryResponse {
        ExtensionApiAgentTaskExecutorInventoryResponse {
            schema: EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            executors: self.executors.clone(),
            failure: self.failure.clone(),
        }
    }

    /// Executors registered by one installed extension, in deterministic order.
    pub fn registered_by<'a>(
        &'a self,
        extension_id: &'a str,
    ) -> impl Iterator<Item = &'a ExtensionApiAgentTaskExecutorDescriptor> + 'a {
        self.executors
            .iter()
            .filter(move |executor| executor.owning_extension == extension_id)
    }
}

fn descriptor(
    extension_id: &str,
    runtime_id: &str,
    declaration: crate::Result<AgentTaskExecutorProviderDeclaration>,
    advertised: &[String],
    unusable: Option<(ExtensionApiAgentTaskExecutorDiagnosticKind, String)>,
) -> ExtensionApiAgentTaskExecutorDescriptor {
    let (id, backend, capabilities, readiness, invalid) = match declaration {
        Ok(declaration) => {
            let capabilities = declaration
                .extra
                .get("capabilities")
                .and_then(serde_json::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let readiness = declaration.readiness_invocation.as_ref().map(|readiness| {
                readiness.timeout_ms.unwrap_or(
                    homeboy_extension_contract::agent_task_executor_declaration::DEFAULT_PROVIDER_READINESS_INVOCATION_TIMEOUT_MS,
                )
            });
            (
                declaration.id,
                declaration.backend,
                capabilities,
                readiness,
                None,
            )
        }
        // A declaration that cannot be parsed has no trustworthy identity, so it
        // is registered as an unnamed unusable entry rather than being silently
        // dropped from the inventory.
        Err(error) => (
            String::new(),
            String::new(),
            Vec::new(),
            None,
            Some(error.message),
        ),
    };

    let capability_advertised = !id.is_empty()
        && advertised.iter().any(|capability| {
            *capability == format!("{AGENT_TASK_EXECUTOR_CAPABILITY_PREFIX}{id}")
        });

    let diagnostic = match (invalid, unusable) {
        (Some(message), _) => Some(ExtensionApiAgentTaskExecutorDiagnostic {
            kind: ExtensionApiAgentTaskExecutorDiagnosticKind::InvalidDeclaration,
            message,
        }),
        (None, Some((kind, message))) => {
            Some(ExtensionApiAgentTaskExecutorDiagnostic { kind, message })
        }
        (None, None) if !capability_advertised => Some(ExtensionApiAgentTaskExecutorDiagnostic {
            kind: ExtensionApiAgentTaskExecutorDiagnosticKind::InvalidDeclaration,
            message: "The extension does not advertise this executor as a capability.".to_string(),
        }),
        (None, None) => None,
    };
    let resolvable = diagnostic.is_none();

    ExtensionApiAgentTaskExecutorDescriptor {
        id,
        backend,
        owning_extension: extension_id.to_string(),
        runtime_id: runtime_id.to_string(),
        capabilities,
        declares_readiness_probe: readiness.is_some(),
        readiness_timeout_ms: readiness,
        resolvable,
        validation: if resolvable {
            ExtensionApiAgentTaskExecutorValidation::Valid
        } else {
            ExtensionApiAgentTaskExecutorValidation::Invalid
        },
        diagnostic,
    }
}

/// An executor id is the selection token, so a colliding id makes every claimant
/// unusable. Failing all of them keeps selection deterministic instead of
/// depending on which extension happened to be discovered first.
fn mark_duplicate_ids(executors: &mut [ExtensionApiAgentTaskExecutorDescriptor]) {
    let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for executor in executors.iter().filter(|executor| executor.resolvable) {
        sources
            .entry(executor.id.clone())
            .or_default()
            .push(format!(
                "runtime:{} source:{}",
                executor.runtime_id, executor.owning_extension
            ));
    }

    for executor in executors.iter_mut() {
        let Some(claimants) = sources.get(&executor.id) else {
            continue;
        };
        if claimants.len() < 2 {
            continue;
        }
        executor.resolvable = false;
        executor.validation = ExtensionApiAgentTaskExecutorValidation::Duplicate;
        executor.diagnostic = Some(ExtensionApiAgentTaskExecutorDiagnostic {
            kind: ExtensionApiAgentTaskExecutorDiagnosticKind::DuplicateId,
            message: format!(
                "Agent-task executor id '{}' is declared by multiple sources: {}. Select one source explicitly before dispatching this executor.",
                executor.id,
                claimants.join(", ")
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declaration(
        value: serde_json::Value,
    ) -> crate::Result<AgentTaskExecutorProviderDeclaration> {
        parse_agent_task_executor_declaration("ext", "runtime", &value)
    }

    #[test]
    fn a_registered_executor_exposes_identity_without_execution_inputs() {
        let advertised = vec![format!(
            "{AGENT_TASK_EXECUTOR_CAPABILITY_PREFIX}opencode.exec"
        )];
        let entry = descriptor(
            "homeboy-opencode",
            "opencode",
            declaration(json!({
                "id": "opencode.exec",
                "backend": "opencode",
                "capabilities": ["workspace_permission_root/v1"],
                "argv": ["opencode-provider", "--secret", "token"],
                "readiness_invocation": { "argv": ["opencode-provider"], "timeout_ms": 5000 }
            })),
            &advertised,
            None,
        );

        assert!(entry.resolvable);
        assert_eq!(entry.backend, "opencode");
        assert_eq!(entry.owning_extension, "homeboy-opencode");
        assert_eq!(entry.runtime_id, "opencode");
        assert!(entry.declares_readiness_probe);
        assert_eq!(entry.readiness_timeout_ms, Some(5000));

        let encoded = serde_json::to_string(&entry).expect("descriptor JSON");
        assert!(
            !encoded.contains("argv")
                && !encoded.contains("opencode-provider")
                && !encoded.contains("token"),
            "registration inventory must not carry execution inputs: {encoded}"
        );
    }

    #[test]
    fn an_unparseable_declaration_is_registered_as_unusable_rather_than_dropped() {
        let entry = descriptor(
            "ext",
            "runtime",
            declaration(json!({ "id": "missing-backend" })),
            &[],
            None,
        );

        assert!(!entry.resolvable);
        assert_eq!(
            entry.diagnostic.expect("diagnostic").kind,
            ExtensionApiAgentTaskExecutorDiagnosticKind::InvalidDeclaration
        );
    }

    #[test]
    fn an_executor_the_extension_does_not_advertise_is_not_resolvable() {
        let entry = descriptor(
            "ext",
            "runtime",
            declaration(json!({ "id": "unadvertised", "backend": "b" })),
            &[],
            None,
        );

        assert!(!entry.resolvable);
    }

    #[test]
    fn an_incompatible_extension_keeps_its_identity_and_reports_why() {
        let advertised = vec![format!("{AGENT_TASK_EXECUTOR_CAPABILITY_PREFIX}exec")];
        let entry = descriptor(
            "ext",
            "runtime",
            declaration(json!({ "id": "exec", "backend": "b" })),
            &advertised,
            Some((
                ExtensionApiAgentTaskExecutorDiagnosticKind::ExtensionIncompatible,
                "too old".to_string(),
            )),
        );

        assert_eq!(entry.id, "exec");
        assert!(!entry.resolvable);
        assert_eq!(
            entry.diagnostic.expect("diagnostic").kind,
            ExtensionApiAgentTaskExecutorDiagnosticKind::ExtensionIncompatible
        );
    }

    #[test]
    fn a_colliding_executor_id_makes_every_claimant_unusable() {
        let advertised = vec![format!("{AGENT_TASK_EXECUTOR_CAPABILITY_PREFIX}shared")];
        let mut executors = vec![
            descriptor(
                "ext-a",
                "runtime-a",
                declaration(json!({ "id": "shared", "backend": "b" })),
                &advertised,
                None,
            ),
            descriptor(
                "ext-b",
                "runtime-b",
                declaration(json!({ "id": "shared", "backend": "b" })),
                &advertised,
                None,
            ),
        ];

        mark_duplicate_ids(&mut executors);

        assert!(executors.iter().all(|executor| !executor.resolvable));
        for executor in &executors {
            let diagnostic = executor.diagnostic.as_ref().expect("duplicate diagnostic");
            assert_eq!(
                diagnostic.kind,
                ExtensionApiAgentTaskExecutorDiagnosticKind::DuplicateId
            );
            // Sources are listed in a stable order so the same collision always
            // produces the same reported conflict.
            assert!(
                diagnostic
                    .message
                    .contains("runtime:runtime-a source:ext-a, runtime:runtime-b source:ext-b"),
                "got {}",
                diagnostic.message
            );
        }
    }

    #[test]
    fn a_unique_executor_survives_duplicate_marking() {
        let advertised = vec![format!("{AGENT_TASK_EXECUTOR_CAPABILITY_PREFIX}only")];
        let mut executors = vec![descriptor(
            "ext",
            "runtime",
            declaration(json!({ "id": "only", "backend": "b" })),
            &advertised,
            None,
        )];

        mark_duplicate_ids(&mut executors);

        assert!(executors[0].resolvable);
    }

    #[test]
    fn an_invalid_request_schema_yields_a_failed_inventory() {
        let api = AgentTaskExecutorApi::discover(&ExtensionApiAgentTaskExecutorInventoryRequest {
            schema: "homeboy/wrong-request/v1".to_string(),
            api_version: EXTENSION_API_V1,
        });

        let response = api.inventory_api();
        assert!(response.executors.is_empty());
        assert!(response.failure.is_some());
    }
}
