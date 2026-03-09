use clap::Args;
use serde::Serialize;

use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct SupportsArgs {
    /// Command path (e.g. "test" or "docs audit")
    pub command: String,

    /// Option/flag to check support for (e.g. --changed-since)
    #[arg(allow_hyphen_values = true)]
    pub option: String,
}

#[derive(Serialize)]
pub struct SupportsOutput {
    pub command: String,
    pub option: String,
    pub supported: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub known_options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

const SUPPORT_MATRIX: &[(&str, &[&str])] = &[
    (
        "test",
        &[
            "--skip-lint",
            "--fix",
            "--coverage",
            "--coverage-min",
            "--baseline",
            "--ignore-baseline",
            "--ratchet",
            "--analyze",
            "--drift",
            "--scaffold",
            "--scaffold-file",
            "--write",
            "--since",
            "--changed-since",
            "--setting",
            "--path",
            "--json-summary",
            "--json",
            "--help",
            "-h",
        ],
    ),
    (
        "audit",
        &[
            "--conventions",
            "--fix",
            "--write",
            "--ratchet",
            "--baseline",
            "--ignore-baseline",
            "--path",
            "--changed-since",
            "--json-summary",
            "--help",
            "-h",
        ],
    ),
    (
        "docs audit",
        &[
            "--path",
            "--docs-dir",
            "--baseline",
            "--ignore-baseline",
            "--features",
            "--help",
            "-h",
        ],
    ),
    (
        "lint",
        &[
            "--fix",
            "--summary",
            "--file",
            "--glob",
            "--changed-only",
            "--changed-since",
            "--errors-only",
            "--sniffs",
            "--exclude-sniffs",
            "--category",
            "--setting",
            "--path",
            "--json",
            "--help",
            "-h",
        ],
    ),
];

pub fn run(args: SupportsArgs, _global: &GlobalArgs) -> CmdResult<SupportsOutput> {
    let command = normalize_command(&args.command);
    let option = args.option.trim().to_string();

    let maybe_entry = SUPPORT_MATRIX
        .iter()
        .find(|(cmd, _)| *cmd == command)
        .map(|(_, opts)| *opts);

    let (supported, known_options, hint) = if let Some(opts) = maybe_entry {
        let known_options = opts.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        let supported = opts.contains(&option.as_str());
        let hint = if supported {
            None
        } else {
            Some(format!(
                "Unsupported option for '{}'. Use `homeboy supports \"{}\" <option>` to probe alternatives.",
                command, command
            ))
        };
        (supported, known_options, hint)
    } else {
        (
            false,
            Vec::new(),
            Some(format!(
                "Unknown command path '{}'. Try one of: {}",
                command,
                SUPPORT_MATRIX
                    .iter()
                    .map(|(cmd, _)| *cmd)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        )
    };

    Ok((
        SupportsOutput {
            command,
            option,
            supported,
            known_options,
            hint,
        },
        if supported { 0 } else { 1 },
    ))
}

fn normalize_command(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
