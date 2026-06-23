use clap::{Args, Subcommand};

// ---------------------------------------------------------------------------
// `git issue` subcommand tree
// ---------------------------------------------------------------------------

#[derive(Args)]
pub struct IssueArgs {
    #[command(subcommand)]
    pub(super) command: IssueCommand,
}

#[derive(Subcommand)]
pub(super) enum IssueCommand {
    /// Create a new issue
    Create {
        /// Component ID
        component_id: String,

        /// Issue title
        #[arg(short, long)]
        title: String,

        /// Issue body (markdown). Prefer --body-file for long content.
        #[arg(short, long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,

        /// Issue label (repeatable)
        #[arg(short, long)]
        label: Vec<String>,

        /// Workspace path to discover the component from a portable homeboy.json
        /// (for unregistered checkouts — CI runners, ad-hoc clones)
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Comment on an existing issue
    Comment {
        /// Component ID
        component_id: String,

        /// Issue number
        #[arg(short, long)]
        number: u64,

        /// Comment body (markdown). Prefer --body-file for long content.
        #[arg(short, long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Find issues matching filters (dedup primitive)
    Find {
        /// Component ID
        component_id: String,

        /// Exact title match
        #[arg(short, long)]
        title: Option<String>,

        /// Required label (repeatable — all labels must be present)
        #[arg(short, long)]
        label: Vec<String>,

        /// State filter: open (default), closed, all
        #[arg(short, long, default_value = "open")]
        state: String,

        /// Max results (default 30)
        #[arg(long, default_value_t = 30)]
        limit: usize,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Close an existing issue with a typed reason
    Close {
        /// Component ID
        component_id: String,

        /// Issue number
        #[arg(short, long)]
        number: u64,

        /// Close reason: completed (default) or not-planned. Use
        /// `not-planned` to suppress re-filing by `homeboy issues reconcile`
        /// — the GitHub-native signal for "we have decided not to fix this."
        #[arg(short, long, default_value = "completed")]
        reason: String,

        /// Optional closing comment (markdown). Posted before the state
        /// transition. Prefer --comment-file for long content.
        #[arg(short, long, conflicts_with = "comment_file")]
        comment: Option<String>,

        /// Read closing comment from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        comment_file: Option<String>,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Edit an existing issue's title, body, or labels
    Edit {
        /// Component ID
        component_id: String,

        /// Issue number
        #[arg(short, long)]
        number: u64,

        /// New title (optional)
        #[arg(short, long)]
        title: Option<String>,

        /// New body (markdown). Prefer --body-file for long content.
        #[arg(short, long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,

        /// Add labels (repeatable)
        #[arg(long = "add-label", value_name = "LABEL")]
        add_labels: Vec<String>,

        /// Remove labels (repeatable)
        #[arg(long = "remove-label", value_name = "LABEL")]
        remove_labels: Vec<String>,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// `git pr` subcommand tree
// ---------------------------------------------------------------------------

#[derive(Args)]
pub struct PrArgs {
    #[command(subcommand)]
    pub(super) command: PrCommand,
}

#[derive(Subcommand)]
pub(super) enum PrCommand {
    /// Create a new pull request
    Create {
        /// Component ID
        component_id: String,

        /// Base branch (target of the PR)
        #[arg(short, long)]
        base: String,

        /// Head branch (source of the PR)
        #[arg(short = 'H', long)]
        head: String,

        /// PR title
        #[arg(short, long)]
        title: String,

        /// PR body (markdown). Prefer --body-file for long content.
        #[arg(short = 'B', long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,

        /// Open as draft
        #[arg(long)]
        draft: bool,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Edit an existing PR's title or body
    Edit {
        /// Component ID
        component_id: String,

        /// PR number
        #[arg(short, long)]
        number: u64,

        /// New title
        #[arg(short, long)]
        title: Option<String>,

        /// New body (markdown)
        #[arg(short = 'B', long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Find PRs matching filters
    Find {
        /// Component ID
        component_id: String,

        /// Base branch filter
        #[arg(short, long)]
        base: Option<String>,

        /// Head branch filter
        #[arg(short = 'H', long)]
        head: Option<String>,

        /// State filter: open (default), closed, merged, all
        #[arg(short, long, default_value = "open")]
        state: String,

        /// Max results (default 30)
        #[arg(long, default_value_t = 30)]
        limit: usize,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Explain PR merge readiness without attempting a merge
    Readiness {
        /// Component ID
        component_id: String,

        /// PR number
        #[arg(short, long)]
        number: u64,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Post a comment on a PR. Three modes:
    ///
    /// 1. Plain: no marker flags — a fresh comment is appended.
    /// 2. Sticky single-section (#1334): `--key <k>` finds-or-updates the one
    ///    comment tagged `<!-- homeboy:key=<k> -->`. The whole `--body` becomes
    ///    the comment body.
    /// 3. Sectioned (#1348): `--comment-key <outer> --section-key <inner>`
    ///    merges `--body` into section `<inner>` of the shared comment tagged
    ///    `<!-- homeboy:comment-key=<outer> -->`. Other sections are preserved.
    ///    `--header` sets the line printed after the outer marker on new
    ///    comments; `--footer` / `--footer-file` sets a block printed after
    ///    the last section (e.g. a tooling-versions <details> block). Both
    ///    are preserved from existing comments on merge when omitted.
    ///    `--section-order` pins section ordering (CSV of keys); default is
    ///    alphabetical.
    ///
    /// Modes 2 and 3 are mutually exclusive. `--key` with `--comment-key` or
    /// `--section-key` is an error.
    Comment {
        /// Component ID
        component_id: String,

        /// PR number
        #[arg(short, long)]
        number: u64,

        /// Comment body (markdown). Prefer --body-file for long content.
        #[arg(short = 'B', long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,

        /// Sticky whole-body key (mode 2, PR #1334).
        /// Mutually exclusive with --comment-key / --section-key.
        #[arg(short, long, conflicts_with_all = ["comment_key", "section_key"])]
        key: Option<String>,

        /// Sectioned mode: outer shared-comment key (mode 3, #1348).
        /// Must be combined with --section-key.
        #[arg(long, requires = "section_key")]
        comment_key: Option<String>,

        /// Sectioned mode: inner per-section key (mode 3, #1348).
        /// Must be combined with --comment-key.
        #[arg(long, requires = "comment_key")]
        section_key: Option<String>,

        /// Sectioned mode: optional header line written after the outer
        /// marker on freshly-created shared comments (e.g.
        /// "## Homeboy Results — `<component>`"). Existing comment headers
        /// are preserved on merge.
        #[arg(long, requires = "comment_key")]
        header: Option<String>,

        /// Sectioned mode: optional footer block written after the last
        /// section (e.g. a tooling-versions <details> block). Existing
        /// footers are preserved on merge when this is omitted; passing
        /// --footer or --footer-file overwrites the preserved footer.
        /// Mutually exclusive with --footer-file.
        #[arg(long, requires = "comment_key", conflicts_with = "footer_file")]
        footer: Option<String>,

        /// Sectioned mode: read footer content from a file ("-" for stdin).
        /// Mutually exclusive with --footer.
        #[arg(long, requires = "comment_key", value_name = "PATH")]
        footer_file: Option<String>,

        /// Sectioned mode: CSV of section keys in desired order. Sections
        /// listed here come first in the given order; others are appended
        /// alphabetically. Example: `--section-order lint,test,audit`.
        #[arg(long, requires = "comment_key", value_delimiter = ',')]
        section_order: Option<Vec<String>>,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Report and optionally land a fleet of pull requests.
    Fleet {
        /// Component ID
        component_id: String,

        /// PR numbers or URLs.
        #[arg(value_name = "PR")]
        refs: Vec<String>,

        /// Update stale PR branches where GitHub can do so safely.
        #[arg(long)]
        update_branches: bool,

        /// Merge green, clean PRs. Without this flag the command is read-only.
        #[arg(long)]
        apply: bool,

        /// Merge method: merge, squash, or rebase.
        #[arg(long, default_value = "squash")]
        merge_method: String,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Compare GitHub mergeability with local git merge-tree evidence.
    ReconcileMergeability {
        /// Component ID
        component_id: String,

        /// PR number
        #[arg(short, long)]
        number: u64,

        /// Workspace path to discover the component from a portable homeboy.json
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Evaluate PR open/merge policy.
    Policy(PrPolicyArgs),
    /// Refresh a PR branch from its current base and report conflicts/checks.
    Refresh {
        /// Component ID
        component_id: String,

        /// PR number or GitHub pull request URL.
        pr: String,

        /// Update strategy. `auto` uses branch/pull rebase git config, falling
        /// back to rebase.
        #[arg(long, default_value = "auto", value_parser = ["auto", "rebase", "merge", "ff-only"])]
        strategy: String,

        /// Push the refreshed PR branch when the worktree is clean and checks pass.
        /// Uses --force-with-lease for rebase safety; plain force is not exposed.
        #[arg(long)]
        push: bool,

        /// Lightweight check command to run after a clean refresh. Repeatable.
        /// Defaults to `git diff --check` when omitted.
        #[arg(long = "check", value_name = "COMMAND")]
        checks: Vec<String>,

        /// Workspace path to discover the component from a portable homeboy.json.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Land a train of ready PRs sequentially, pausing on the first blocker.
    Land {
        /// Repository as owner/repo or host/owner/repo.
        repo: String,

        /// PR numbers or URLs. URLs must point at the selected repo.
        #[arg(value_name = "PR")]
        prs: Vec<String>,

        /// Merge method: merge, squash, or rebase.
        #[arg(long, default_value = "squash", value_parser = ["merge", "squash", "rebase"])]
        merge_method: String,

        /// Delete the PR branch after merge.
        #[arg(long)]
        delete_branch: bool,

        /// Inspect and report what would land without merging or refreshing.
        #[arg(long)]
        dry_run: bool,

        /// Safe helper program used to refresh a dirty dependent PR.
        /// Not run through a shell. Combine with --refresh-helper-arg.
        #[arg(long, value_name = "PROGRAM")]
        refresh_helper: Option<String>,

        /// Argument for --refresh-helper. Supports {repo}, {number}, {url}, {head_sha}.
        #[arg(
            long = "refresh-helper-arg",
            value_name = "ARG",
            requires = "refresh_helper"
        )]
        refresh_helper_args: Vec<String>,

        /// Retry merge after this many base-branch-modified races.
        #[arg(long, default_value_t = 1)]
        max_base_retries: usize,
    },
}

#[derive(Args)]
pub struct PrPolicyArgs {
    #[command(subcommand)]
    pub(super) command: PrPolicyCommand,
}

#[derive(Subcommand)]
pub(super) enum PrPolicyCommand {
    /// Evaluate whether Homeboy may create or update a proposed PR.
    Open {
        /// Component ID
        component_id: String,

        /// Policy file path (YAML or JSON)
        #[arg(long, value_name = "PATH")]
        policy: String,

        /// Change source, e.g. autofix, deps, generated, release-prep, agent.
        #[arg(long)]
        source: Option<String>,

        /// Base branch.
        #[arg(long)]
        base: Option<String>,

        /// Head branch.
        #[arg(long)]
        head: Option<String>,

        /// Head repository owner/name.
        #[arg(long = "head-repo")]
        head_repository: Option<String>,

        /// Base repository owner/name.
        #[arg(long)]
        repository: Option<String>,

        /// Changed file path. Repeatable.
        #[arg(long = "file", value_name = "PATH")]
        files: Vec<String>,

        /// Read changed file paths from a newline-delimited file.
        #[arg(long, value_name = "PATH")]
        files_file: Option<String>,

        /// Read changed files from the current git working tree.
        #[arg(long)]
        files_from_git: bool,

        /// Workspace path to discover the component from a portable homeboy.json.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Evaluate whether an existing PR is safe to merge; optionally merge it.
    Merge {
        /// Component ID
        component_id: String,

        /// Policy file path (YAML or JSON)
        #[arg(long, value_name = "PATH")]
        policy: String,

        /// PR number.
        #[arg(short, long)]
        number: u64,

        /// Author login override. Defaults to GitHub PR metadata.
        #[arg(long)]
        author: Option<String>,

        /// Base branch override. Defaults to GitHub PR metadata.
        #[arg(long)]
        base: Option<String>,

        /// Head branch override. Defaults to GitHub PR metadata.
        #[arg(long)]
        head: Option<String>,

        /// Head repository owner/name override. Defaults to GitHub PR metadata.
        #[arg(long = "head-repo")]
        head_repository: Option<String>,

        /// Base repository owner/name override. Defaults to component remote.
        #[arg(long)]
        repository: Option<String>,

        /// Merge the PR when policy allows it.
        #[arg(long)]
        merge: bool,

        /// Merge method: merge, squash, or rebase.
        #[arg(long, default_value = "squash")]
        merge_method: String,

        /// Workspace path to discover the component from a portable homeboy.json.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
}
