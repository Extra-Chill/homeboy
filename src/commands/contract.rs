use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::{json, Value};

use crate::command_contract::{
    registered_contract, registered_contracts, CommandDispatchFamily, CommandJsonFamily,
    CommandOutputContractKind, CommandOutputFileMode, CommandRawOutputMode, CommandResponseMode,
    CommandStdoutMode, ContractRegistryEntry, COMMAND_REGISTRY, PUBLIC_OUTPUT_VARIANT_CONTRACTS,
    RUNNER_ARTIFACT_MANIFEST_SCHEMA, RUNNER_HANDOFF_ENVELOPE_SCHEMA, RUNNER_WORKLOAD_SCHEMA,
    RUN_LOCATION_INDEX_SCHEMA,
};
use crate::commands::{CmdResult, GlobalArgs};

const CONTRACT_EXPORT_INDEX_SCHEMA: &str = "homeboy/contract-export-index/v1";
const COMMAND_REGISTRY_EXPORT_SCHEMA: &str = "homeboy/command-registry-export/v1";
const PUBLIC_OUTPUT_VARIANTS_EXPORT_SCHEMA: &str = "homeboy/public-output-variants-export/v1";
const CONTRACT_SCHEMA_CATALOG_SCHEMA: &str = "homeboy/contract-schema-catalog/v1";

#[derive(Args, Debug, Clone)]
pub struct ContractArgs {
    #[command(subcommand)]
    pub command: ContractCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ContractCommand {
    /// List core-owned data contracts.
    List,
    /// Show one core-owned data contract by schema id or registry name.
    Show(ContractShowArgs),
    /// Export machine-consumable Homeboy contract JSON files.
    Export(ContractExportArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ContractShowArgs {
    /// Schema id or short registry name.
    pub schema_id_or_name: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ContractOutput {
    Export(ContractExportOutput),
    List(ContractListOutput),
    Show(ContractShowOutput),
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ContractListOutput {
    pub kind: &'static str,
    pub contracts: Vec<ContractListEntry>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ContractShowOutput {
    pub kind: &'static str,
    pub contract: ContractDetail,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ContractListEntry {
    pub schema_id: &'static str,
    pub name: &'static str,
    pub title: &'static str,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ContractDetail {
    pub schema_id: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub owner: &'static str,
    pub summary: &'static str,
    pub rust_type: &'static str,
}

impl From<&'static ContractRegistryEntry> for ContractListEntry {
    fn from(entry: &'static ContractRegistryEntry) -> Self {
        Self {
            schema_id: entry.schema_id,
            name: entry.name,
            title: entry.title,
        }
    }
}

impl From<&'static ContractRegistryEntry> for ContractDetail {
    fn from(entry: &'static ContractRegistryEntry) -> Self {
        Self {
            schema_id: entry.schema_id,
            name: entry.name,
            title: entry.title,
            owner: entry.owner,
            summary: entry.summary,
            rust_type: entry.rust_type,
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct ContractExportArgs {
    /// Directory to receive exported JSON contract files.
    #[arg(long, value_name = "DIR")]
    pub dir: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct ContractExportOutput {
    pub command: String,
    pub schema: &'static str,
    pub version: u32,
    pub description: &'static str,
    pub dir: String,
    pub files: Vec<ContractExportFile>,
}

#[derive(Debug, Serialize)]
pub struct ContractExportFile {
    pub path: String,
    pub schema: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Serialize)]
struct CommandRegistryExport {
    schema: &'static str,
    version: u32,
    description: &'static str,
    fields: Vec<FieldMetadata>,
    required: Vec<&'static str>,
    commands: Vec<CommandContractExport>,
}

#[derive(Debug, Serialize)]
struct CommandContractExport {
    name: &'static str,
    json_family: &'static str,
    dispatch_family: &'static str,
    docs_path: Option<String>,
    safety: CommandSafetyExport,
    output: CommandOutputExport,
    lab: CommandLabExport,
}

#[derive(Debug, Serialize)]
struct CommandSafetyExport {
    mutates: bool,
    operator: bool,
    dry_run_flag: Option<&'static str>,
    risk_exemption: Option<&'static str>,
    dangerous_flags: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct CommandOutputExport {
    response_mode: &'static str,
    stdout_mode: &'static str,
    output_file_mode: &'static str,
    json_family: &'static str,
    output_contract: &'static str,
    notes: &'static str,
}

#[derive(Debug, Serialize)]
struct CommandLabExport {
    supported: bool,
    notes: &'static str,
    support_summary: Vec<CommandLabSupportExport>,
}

#[derive(Debug, Serialize)]
struct CommandLabSupportExport {
    contract_labels: Vec<&'static str>,
    message_label: &'static str,
    hint_label: &'static str,
}

#[derive(Debug, Serialize)]
struct PublicOutputVariantsExport {
    schema: &'static str,
    version: u32,
    description: &'static str,
    fields: Vec<FieldMetadata>,
    required: Vec<&'static str>,
    variants: Vec<PublicOutputVariantExport>,
}

#[derive(Debug, Serialize)]
struct PublicOutputVariantExport {
    command: &'static str,
    variant: &'static str,
    discriminator_field: Option<&'static str>,
    discriminator_value: Option<&'static str>,
    golden_fixture: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct ContractSchemaCatalog {
    schema: &'static str,
    version: u32,
    description: &'static str,
    contracts: Vec<ContractSchemaEntry>,
}

#[derive(Debug, Serialize)]
struct ContractSchemaEntry {
    id: &'static str,
    version: u32,
    description: &'static str,
    fields: Vec<FieldMetadata>,
    required: Vec<&'static str>,
    example: Value,
}

#[derive(Debug, Serialize)]
struct FieldMetadata {
    name: &'static str,
    kind: &'static str,
    description: &'static str,
    required: bool,
}

pub fn run(args: ContractArgs, _global: &GlobalArgs) -> CmdResult<ContractOutput> {
    match args.command {
        ContractCommand::List => Ok((
            ContractOutput::List(ContractListOutput {
                kind: "list",
                contracts: registered_contracts()
                    .iter()
                    .map(ContractListEntry::from)
                    .collect(),
            }),
            0,
        )),
        ContractCommand::Show(args) => {
            let contract = registered_contract(&args.schema_id_or_name).ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "schema_id_or_name",
                    format!("unknown Homeboy core contract `{}`", args.schema_id_or_name),
                    Some(args.schema_id_or_name.clone()),
                    Some(vec![
                        "Run `homeboy contract list` to inspect registered contracts.".to_string(),
                    ]),
                )
            })?;

            Ok((
                ContractOutput::Show(ContractShowOutput {
                    kind: "show",
                    contract: contract.into(),
                }),
                0,
            ))
        }
        ContractCommand::Export(args) => {
            export_contracts(args).map(|(output, code)| (ContractOutput::Export(output), code))
        }
    }
}

fn export_contracts(args: ContractExportArgs) -> CmdResult<ContractExportOutput> {
    fs::create_dir_all(&args.dir).map_err(|error| io_error(error, &args.dir))?;

    let exports = [
        ExportDocument {
            file_name: "command-registry.json",
            schema: COMMAND_REGISTRY_EXPORT_SCHEMA,
            description: "Top-level command registry metadata derived from Homeboy's command contract registry.",
            value: serde_json::to_value(command_registry_export()).map_err(json_error)?,
        },
        ExportDocument {
            file_name: "public-output-variants.json",
            schema: PUBLIC_OUTPUT_VARIANTS_EXPORT_SCHEMA,
            description: "Public command output variants and their discriminator/golden fixture anchors.",
            value: serde_json::to_value(public_output_variants_export()).map_err(json_error)?,
        },
        ExportDocument {
            file_name: "schema-catalog.json",
            schema: CONTRACT_SCHEMA_CATALOG_SCHEMA,
            description: "Homeboy-owned contract schema IDs with field metadata and canonical examples.",
            value: serde_json::to_value(contract_schema_catalog()).map_err(json_error)?,
        },
    ];

    let mut files = Vec::new();
    for export in exports {
        let path = args.dir.join(export.file_name);
        let body = serde_json::to_string_pretty(&export.value).map_err(json_error)?;
        fs::write(&path, format!("{body}\n")).map_err(|error| io_error(error, &path))?;
        files.push(ContractExportFile {
            path: path.to_string_lossy().to_string(),
            schema: export.schema,
            description: export.description,
        });
    }

    Ok((
        ContractExportOutput {
            command: "contract.export".to_string(),
            schema: CONTRACT_EXPORT_INDEX_SCHEMA,
            version: 1,
            description: "Machine-consumable Homeboy-owned contract exports.",
            dir: args.dir.to_string_lossy().to_string(),
            files,
        },
        0,
    ))
}

struct ExportDocument {
    file_name: &'static str,
    schema: &'static str,
    description: &'static str,
    value: Value,
}

fn command_registry_export() -> CommandRegistryExport {
    CommandRegistryExport {
        schema: COMMAND_REGISTRY_EXPORT_SCHEMA,
        version: 1,
        description: "Top-level Homeboy command metadata for downstream consumers that need stable command constants without linking Rust.",
        fields: vec![
            field("name", "string", "Top-level CLI command name.", true),
            field("json_family", "string", "Structured output family used by the command.", true),
            field("dispatch_family", "string", "Runtime JSON dispatch family.", true),
            field("docs_path", "string|null", "Repository-relative documentation path when present.", false),
            field("safety", "object", "Mutation/operator safety metadata.", true),
            field("output", "object", "Output routing and contract metadata.", true),
            field("lab", "object", "Lab runner support metadata.", true),
        ],
        required: vec!["name", "json_family", "dispatch_family", "safety", "output", "lab"],
        commands: COMMAND_REGISTRY
            .iter()
            .map(|entry| CommandContractExport {
                name: entry.name,
                json_family: json_family(entry.json_family),
                dispatch_family: dispatch_family(entry.dispatch_family()),
                docs_path: entry.docs_path(),
                safety: CommandSafetyExport {
                    mutates: entry.safety.mutates,
                    operator: entry.safety.operator,
                    dry_run_flag: entry.safety.dry_run_flag,
                    risk_exemption: entry.safety.risk_exemption,
                    dangerous_flags: entry.safety.dangerous_flags.to_vec(),
                },
                output: CommandOutputExport {
                    response_mode: response_mode(CommandResponseMode::Json),
                    stdout_mode: stdout_mode(CommandStdoutMode::JsonEnvelope),
                    output_file_mode: output_file_mode(CommandOutputFileMode::GenericEnvelope),
                    json_family: json_family(entry.json_family),
                    output_contract: output_contract(CommandOutputContractKind::JsonEnvelope),
                    notes: entry.output_notes,
                },
                lab: CommandLabExport {
                    supported: entry.lab_supported,
                    notes: entry.lab_notes,
                    support_summary: entry
                        .lab_support_summary
                        .iter()
                        .map(|summary| CommandLabSupportExport {
                            contract_labels: summary.contract_labels.to_vec(),
                            message_label: summary.message_label,
                            hint_label: summary.hint_label,
                        })
                        .collect(),
                },
            })
            .collect(),
    }
}

fn public_output_variants_export() -> PublicOutputVariantsExport {
    PublicOutputVariantsExport {
        schema: PUBLIC_OUTPUT_VARIANTS_EXPORT_SCHEMA,
        version: 1,
        description: "Public output variant discriminators and golden fixtures used by downstream contract tests.",
        fields: vec![
            field("command", "string", "Top-level command that owns the variant.", true),
            field("variant", "string", "Stable variant name.", true),
            field("discriminator_field", "string|null", "Field used to identify this variant in JSON output.", false),
            field("discriminator_value", "string|null", "Expected discriminator value.", false),
            field("golden_fixture", "string|null", "Committed golden fixture file when one anchors the wire shape.", false),
        ],
        required: vec!["command", "variant"],
        variants: PUBLIC_OUTPUT_VARIANT_CONTRACTS
            .iter()
            .map(|contract| PublicOutputVariantExport {
                command: contract.command,
                variant: contract.variant,
                discriminator_field: contract.discriminator_field,
                discriminator_value: contract.discriminator_value,
                golden_fixture: contract.golden_fixture,
            })
            .collect(),
    }
}

fn contract_schema_catalog() -> ContractSchemaCatalog {
    ContractSchemaCatalog {
        schema: CONTRACT_SCHEMA_CATALOG_SCHEMA,
        version: 1,
        description:
            "Homeboy-owned schema IDs and canonical examples for cross-language contract tests.",
        contracts: vec![
            ContractSchemaEntry {
                id: RUNNER_WORKLOAD_SCHEMA,
                version: 1,
                description: "Runner-resident workload request consumed by Lab runners.",
                fields: vec![
                    field(
                        "schema",
                        "string",
                        "Schema ID for the workload envelope.",
                        true,
                    ),
                    field("workload_id", "string", "Stable workload identifier.", true),
                    field("kind", "object", "Command label and command family.", true),
                    field(
                        "workspace_mappings",
                        "object",
                        "Source/workspace materialization policy.",
                        true,
                    ),
                    field(
                        "required_capabilities",
                        "array",
                        "Runner capabilities required by the workload.",
                        true,
                    ),
                    field(
                        "required_secrets",
                        "object",
                        "Secret categories required by the workload.",
                        true,
                    ),
                    field(
                        "mutation_policy",
                        "object",
                        "Patch capture and dirty-workspace policy.",
                        true,
                    ),
                    field("assignment", "object", "Runner assignment metadata.", true),
                    field("state", "object", "Current runner-side state.", true),
                    field(
                        "result_refs",
                        "object",
                        "Result and artifact references.",
                        true,
                    ),
                ],
                required: vec![
                    "schema",
                    "workload_id",
                    "kind",
                    "workspace_mappings",
                    "required_capabilities",
                    "required_secrets",
                    "mutation_policy",
                    "assignment",
                    "state",
                    "result_refs",
                ],
                example: runner_workload_example(),
            },
            ContractSchemaEntry {
                id: RUNNER_HANDOFF_ENVELOPE_SCHEMA,
                version: 1,
                description: "Detached Lab offload handoff envelope returned to the controller.",
                fields: vec![
                    field(
                        "schema",
                        "string",
                        "Schema ID for the handoff envelope.",
                        true,
                    ),
                    field("status", "string", "Handoff status.", true),
                    field(
                        "execution_location",
                        "string",
                        "Runner execution location.",
                        true,
                    ),
                    field("runner_id", "string", "Selected runner ID.", true),
                    field("job_id", "string", "Runner job ID.", true),
                    field(
                        "remote_cwd",
                        "string",
                        "Runner-side working directory.",
                        true,
                    ),
                    field(
                        "artifact_manifest",
                        "object",
                        "Artifact manifest reference.",
                        true,
                    ),
                    field(
                        "follow_commands",
                        "object",
                        "Operator follow-up commands.",
                        true,
                    ),
                ],
                required: vec![
                    "schema",
                    "status",
                    "execution_location",
                    "runner_id",
                    "job_id",
                    "remote_cwd",
                    "artifact_manifest",
                    "follow_commands",
                ],
                example: runner_handoff_example(),
            },
            ContractSchemaEntry {
                id: RUN_LOCATION_INDEX_SCHEMA,
                version: 1,
                description: "Controller-side pointer from a run to its runner execution location.",
                fields: vec![
                    field(
                        "schema",
                        "string",
                        "Schema ID for the run location index.",
                        true,
                    ),
                    field("run_id", "string", "Controller-visible run ID.", true),
                    field(
                        "controller_location",
                        "string",
                        "Controller origin label.",
                        true,
                    ),
                    field("runner_id", "string", "Selected runner ID.", true),
                    field("remote_job_id", "string", "Runner job ID.", true),
                    field(
                        "artifact_manifest_ref",
                        "object",
                        "Runner artifact manifest pointer.",
                        true,
                    ),
                    field(
                        "liveness_heartbeat_timestamp",
                        "string",
                        "Last heartbeat timestamp.",
                        true,
                    ),
                ],
                required: vec![
                    "schema",
                    "run_id",
                    "controller_location",
                    "runner_id",
                    "remote_job_id",
                    "artifact_manifest_ref",
                    "liveness_heartbeat_timestamp",
                ],
                example: run_location_index_example(),
            },
        ],
    }
}

fn runner_workload_example() -> Value {
    json!({
        "schema": RUNNER_WORKLOAD_SCHEMA,
        "workload_id": "workload-1",
        "kind": { "command_label": "test", "command_family": "quality" },
        "workspace_mappings": {
            "source_path_mode": "cwd_or_path_flag",
            "workspace_mode_policy": "git",
            "mapping_ref": "workspace-1"
        },
        "required_capabilities": [{ "name": "cargo", "required": true }],
        "required_secrets": { "categories": [] },
        "required_extensions": [],
        "required_extension_revisions": [],
        "mutation_policy": {
            "capture_patch": false,
            "mutation_flag": null,
            "allow_dirty_lab_workspace": false
        },
        "assignment": { "runner_id": "runner-1", "runner_mode": "lab", "source": "selected" },
        "state": { "status": "queued", "remote_workspace": null, "fallback_reason": null },
        "result_refs": {
            "plan_id": "plan-1",
            "proof_id": null,
            "workspace_mapping_ref": "workspace-1",
            "artifacts": []
        }
    })
}

fn runner_handoff_example() -> Value {
    json!({
        "schema": RUNNER_HANDOFF_ENVELOPE_SCHEMA,
        "status": "handoff_complete",
        "execution_location": "runner:runner-1",
        "identity": {
            "runner_id": "runner-1",
            "runner_job_id": "job-1",
            "persisted_run_id": "run-1",
            "run_id": "run-1",
            "handoff_id": "runner:runner-1:job:job-1"
        },
        "runner_id": "runner-1",
        "job_id": "job-1",
        "durable_run_id": "run-1",
        "persisted_run_id": "run-1",
        "mirror_run_id": "run-1",
        "remote_cwd": "/home/runner/workspace",
        "artifact_manifest": {
            "schema": "homeboy/runner-artifact-manifest-ref/v1",
            "manifest_schema": RUNNER_ARTIFACT_MANIFEST_SCHEMA,
            "path": "/home/runner/workspace-homeboy-artifacts/manifest.json"
        },
        "follow_commands": {
            "job_logs": "homeboy runner job logs runner-1 job-1 --follow",
            "job_cancel": "homeboy runner job cancel runner-1 job-1",
            "status": "homeboy agent-task status run-1",
            "logs": "homeboy agent-task logs run-1",
            "artifacts": "homeboy agent-task artifacts run-1"
        }
    })
}

fn run_location_index_example() -> Value {
    json!({
        "schema": RUN_LOCATION_INDEX_SCHEMA,
        "run_id": "run-1",
        "controller_location": "controller:local",
        "runner_id": "runner-1",
        "remote_job_id": "job-1",
        "artifact_manifest_ref": {
            "schema": "homeboy/runner-artifact-manifest-ref/v1",
            "manifest_schema": RUNNER_ARTIFACT_MANIFEST_SCHEMA,
            "path": "/home/runner/workspace-homeboy-artifacts/manifest.json"
        },
        "liveness_heartbeat_timestamp": "2026-01-01T00:00:00Z"
    })
}

fn field(
    name: &'static str,
    kind: &'static str,
    description: &'static str,
    required: bool,
) -> FieldMetadata {
    FieldMetadata {
        name,
        kind,
        description,
        required,
    }
}

fn json_family(value: CommandJsonFamily) -> &'static str {
    match value {
        CommandJsonFamily::Quality => "quality",
        CommandJsonFamily::Workspace => "workspace",
        CommandJsonFamily::Ops => "ops",
        CommandJsonFamily::RawOnly => "raw_only",
    }
}

fn dispatch_family(value: CommandDispatchFamily) -> &'static str {
    match value {
        CommandDispatchFamily::Quality => "quality",
        CommandDispatchFamily::Workspace => "workspace",
        CommandDispatchFamily::Ops => "ops",
        CommandDispatchFamily::RawOnly => "raw_only",
    }
}

fn response_mode(value: CommandResponseMode) -> &'static str {
    match value {
        CommandResponseMode::Json => "json",
        CommandResponseMode::Raw(mode) => raw_output_mode(mode),
    }
}

fn stdout_mode(value: CommandStdoutMode) -> &'static str {
    match value {
        CommandStdoutMode::JsonEnvelope => "json_envelope",
        CommandStdoutMode::Raw(mode) => raw_output_mode(mode),
    }
}

fn raw_output_mode(value: CommandRawOutputMode) -> &'static str {
    match value {
        CommandRawOutputMode::InteractivePassthrough => "interactive_passthrough",
        CommandRawOutputMode::Markdown => "markdown",
        CommandRawOutputMode::PlainText => "plain_text",
    }
}

fn output_file_mode(value: CommandOutputFileMode) -> &'static str {
    match value {
        CommandOutputFileMode::None => "none",
        CommandOutputFileMode::GenericEnvelope => "generic_envelope",
        CommandOutputFileMode::ReviewStableArtifact => "review_stable_artifact",
        CommandOutputFileMode::TraceJsonSummaryArtifact => "trace_json_summary_artifact",
    }
}

fn output_contract(value: CommandOutputContractKind) -> &'static str {
    match value {
        CommandOutputContractKind::JsonEnvelope => "json_envelope",
        CommandOutputContractKind::RawOnly => "raw_only",
    }
}

fn io_error(error: std::io::Error, path: &Path) -> homeboy::core::Error {
    homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
}

fn json_error(error: serde_json::Error) -> homeboy::core::Error {
    homeboy::core::Error::internal_json(error.to_string(), Some("export contracts".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{current_command_surface, Commands};

    #[test]
    fn command_registry_export_covers_contract_command() {
        let export = command_registry_export();

        assert!(export
            .commands
            .iter()
            .any(|command| command.name == "contract"));
        assert!(current_command_surface().contains_path(&["contract", "export"]));
    }

    #[test]
    fn contract_export_writes_stable_json_files() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let args = ContractArgs {
            command: ContractCommand::Export(ContractExportArgs {
                dir: tempdir.path().to_path_buf(),
            }),
        };

        let (output, exit_code) = run(args, &GlobalArgs {}).expect("contract export");

        assert_eq!(exit_code, 0);
        let ContractOutput::Export(output) = output else {
            panic!("expected export output");
        };
        assert_eq!(output.schema, CONTRACT_EXPORT_INDEX_SCHEMA);
        assert_eq!(output.files.len(), 3);

        let registry: Value = serde_json::from_str(
            &fs::read_to_string(tempdir.path().join("command-registry.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(registry["schema"], COMMAND_REGISTRY_EXPORT_SCHEMA);
        assert!(registry["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command["name"] == "contract"));

        let catalog: Value = serde_json::from_str(
            &fs::read_to_string(tempdir.path().join("schema-catalog.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog["schema"], CONTRACT_SCHEMA_CATALOG_SCHEMA);
        assert!(catalog["contracts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|contract| contract["id"] == RUNNER_WORKLOAD_SCHEMA));
    }

    #[test]
    fn contract_command_name_is_registered_for_json_dispatch() {
        let command = Commands::Contract(ContractArgs {
            command: ContractCommand::Export(ContractExportArgs {
                dir: PathBuf::from("contracts"),
            }),
        });

        assert_eq!(command.top_level_name(), "contract");
    }

    #[test]
    fn list_returns_registered_contracts() {
        let (output, exit_code) = run(
            ContractArgs {
                command: ContractCommand::List,
            },
            &GlobalArgs {},
        )
        .expect("list contracts");

        assert_eq!(exit_code, 0);
        let ContractOutput::List(output) = output else {
            panic!("expected list output");
        };
        assert_eq!(output.kind, "list");
        assert!(output
            .contracts
            .iter()
            .any(|contract| contract.name == "secret-env-plan"));
    }

    #[test]
    fn show_resolves_by_name() {
        let (output, exit_code) = run(
            ContractArgs {
                command: ContractCommand::Show(ContractShowArgs {
                    schema_id_or_name: "secret-env-plan".to_string(),
                }),
            },
            &GlobalArgs {},
        )
        .expect("show contract");

        assert_eq!(exit_code, 0);
        let ContractOutput::Show(output) = output else {
            panic!("expected show output");
        };
        assert_eq!(output.kind, "show");
        assert_eq!(
            output.contract.schema_id,
            crate::core::secret_env_plan::SECRET_ENV_PLAN_SCHEMA
        );
    }

    #[test]
    fn show_rejects_unknown_contract() {
        let err = run(
            ContractArgs {
                command: ContractCommand::Show(ContractShowArgs {
                    schema_id_or_name: "missing-contract".to_string(),
                }),
            },
            &GlobalArgs {},
        )
        .expect_err("unknown contract should fail");

        assert!(err.to_string().contains("missing-contract"));
    }
}
