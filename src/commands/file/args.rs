use clap::{Args, Subcommand};

use homeboy::core::server::transfer::TransferConfig;

/// Inspect and modify remote project files.
///
/// Path resolution mirrors deploy so inspection agrees with the deployed path:
/// absolute paths are used verbatim; relative paths matching a managed prefix
/// declared by a linked extension (e.g. `wp-content/...`) resolve through the
/// project's configured `path_roots` (the same root deploy writes active
/// components to); everything else joins against the project `base_path`.
#[derive(Args)]
pub struct FileArgs {
    #[command(subcommand)]
    pub(crate) command: FileCommand,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum FileCommand {
    /// List directory contents
    List {
        /// Project ID
        project_id: String,
        /// Remote directory path
        path: String,
    },
    /// Read file content
    Read {
        /// Project ID
        project_id: String,
        /// Remote file path
        path: String,
        /// Output raw content only (no JSON wrapper)
        #[arg(long)]
        raw: bool,
    },
    /// Write content to file (from stdin)
    Write {
        /// Project ID
        project_id: String,
        /// Remote file path
        path: String,
        /// Apply the destructive write. Without this flag, prints a plan only.
        #[arg(long)]
        apply: bool,
    },
    /// Create a directory
    Mkdir {
        /// Project ID
        project_id: String,
        /// Remote directory path
        path: String,
    },
    /// Delete a file or directory
    Delete {
        /// Project ID
        project_id: String,
        /// Remote path to delete
        path: String,
        /// Delete directories recursively
        #[arg(short, long)]
        recursive: bool,
        /// Apply the destructive delete. Without this flag, prints a plan only.
        #[arg(long)]
        apply: bool,
    },
    /// Rename or move a file
    Rename {
        /// Project ID
        project_id: String,
        /// Current path
        old_path: String,
        /// New path
        new_path: String,
    },
    /// Find files by name pattern
    Find {
        /// Project ID
        project_id: String,
        /// Directory path to search
        path: String,
        /// Filename pattern (glob, e.g., "*.php")
        #[arg(long)]
        name: Option<String>,
        /// File type: f (file), d (directory), l (symlink)
        #[arg(long, name = "type")]
        file_type: Option<String>,
        /// Maximum directory depth
        #[arg(long)]
        max_depth: Option<u32>,
    },
    /// Search file contents
    Grep {
        /// Project ID
        project_id: String,
        /// Directory path to search
        path: String,
        /// Search pattern
        pattern: String,
        /// Filter files by name pattern (e.g., "*.php")
        #[arg(long)]
        name: Option<String>,
        /// Maximum directory depth
        #[arg(long)]
        max_depth: Option<u32>,
        /// Case insensitive search
        #[arg(short = 'i', long)]
        ignore_case: bool,
    },
    /// Download a file or directory from remote server
    Download {
        /// Project ID
        project_id: String,
        /// Remote file path
        path: String,
        /// Local destination path (defaults to current directory)
        #[arg(default_value = ".")]
        local_path: String,
        /// Download directories recursively
        #[arg(short, long)]
        recursive: bool,
    },
    /// Copy a file or path between local and remote targets
    Copy(TransferArgs),
    /// Sync a directory between local and remote targets without deleting extras
    Sync(SyncArgs),
    /// Edit file with line-based or pattern-based operations
    Edit(EditArgs),
}

#[derive(Args)]
pub(crate) struct TransferArgs {
    /// Source: local path or server_id:/path
    source: String,
    /// Destination: local path or server_id:/path
    destination: String,
    /// Copy directories recursively
    #[arg(short, long)]
    recursive: bool,
    #[command(flatten)]
    flags: TransferFlags,
}

#[derive(Args)]
pub(crate) struct SyncArgs {
    /// Source: local path or server_id:/path
    source: String,
    /// Destination: local path or server_id:/path
    destination: String,
    #[command(flatten)]
    flags: TransferFlags,
}

#[derive(Args)]
pub(crate) struct TransferFlags {
    /// Compress data during transfer
    #[arg(short, long)]
    compress: bool,
    /// Show what would be copied without doing it
    #[arg(long)]
    dry_run: bool,
    /// Exclude patterns for recursive server-to-server copies
    #[arg(long)]
    exclude: Vec<String>,
}

impl TransferArgs {
    pub(crate) fn into_config(self) -> TransferConfig {
        transfer_config(self.source, self.destination, self.recursive, self.flags)
    }
}

impl SyncArgs {
    pub(crate) fn into_config(self) -> TransferConfig {
        transfer_config(self.source, self.destination, true, self.flags)
    }
}

fn transfer_config(
    source: String,
    destination: String,
    recursive: bool,
    flags: TransferFlags,
) -> TransferConfig {
    TransferConfig {
        source,
        destination,
        recursive,
        compress: flags.compress,
        dry_run: flags.dry_run,
        exclude: flags.exclude,
    }
}

#[derive(Args)]
pub(crate) struct EditArgs {
    /// Project ID
    pub(crate) project_id: String,
    /// Remote file path
    pub(crate) file_path: String,
    /// Show changes without applying
    #[arg(short = 'n', long)]
    pub(crate) dry_run: bool,
    /// Apply even if multiple pattern matches (warns by default)
    #[arg(short, long)]
    pub(crate) force: bool,
    #[command(flatten)]
    pub(crate) line_ops: LineOperations,
    #[command(flatten)]
    pub(crate) pattern_ops: PatternOperations,
    #[command(flatten)]
    pub(crate) file_mods: FileModifications,
}

#[derive(Args, Default)]
pub(crate) struct LineOperations {
    #[arg(long)]
    pub(crate) replace_line: Option<usize>,
    #[arg(long, value_name = "CONTENT", requires = "replace_line")]
    pub(crate) replace_line_content: Option<String>,
    #[arg(long)]
    pub(crate) insert_after: Option<usize>,
    #[arg(long, value_name = "CONTENT", requires = "insert_after")]
    pub(crate) insert_after_content: Option<String>,
    #[arg(long)]
    pub(crate) insert_before: Option<usize>,
    #[arg(long, value_name = "CONTENT", requires = "insert_before")]
    pub(crate) insert_before_content: Option<String>,
    #[arg(long)]
    pub(crate) delete_line: Option<usize>,
    #[arg(long, value_names = ["START", "END"])]
    pub(crate) delete_lines: Option<Vec<usize>>,
}

#[derive(Args, Default)]
pub(crate) struct PatternOperations {
    #[arg(long, value_name = "PATTERN")]
    pub(crate) replace_pattern: Option<String>,
    #[arg(long, value_name = "CONTENT", requires = "replace_pattern")]
    pub(crate) replace_pattern_content: Option<String>,
    #[arg(long)]
    pub(crate) replace_all_pattern: Option<String>,
    #[arg(long, value_name = "CONTENT", requires = "replace_all_pattern")]
    pub(crate) replace_all_content: Option<String>,
    #[arg(long, value_name = "PATTERN")]
    pub(crate) delete_pattern: Option<String>,
}

#[derive(Args, Default)]
pub(crate) struct FileModifications {
    #[arg(long, value_name = "CONTENT")]
    pub(crate) append: Option<String>,
    #[arg(long, value_name = "CONTENT")]
    pub(crate) prepend: Option<String>,
}
