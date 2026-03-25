//! read — extracted from file.rs.

use super::super::CmdResult;
use super::FileArgs;
use super::FileCommand;
use super::FileOutput;
use clap::{Args, Subcommand};
use homeboy::project::files::{self, FileEntry, GrepMatch, LineChange};
use serde::Serialize;

pub fn is_raw_read(args: &FileArgs) -> bool {
    matches!(&args.command, FileCommand::Read { raw: true, .. })
}

pub(crate) fn read(project_id: &str, path: &str) -> CmdResult<FileOutput> {
    let result = files::read(project_id, path)?;

    Ok((
        FileOutput {
            command: "file.read".to_string(),
            project_id: project_id.to_string(),
            base_path: result.base_path,
            path: Some(result.path),
            old_path: None,
            new_path: None,
            recursive: None,
            entries: None,
            content: Some(result.content),
            bytes_written: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}
