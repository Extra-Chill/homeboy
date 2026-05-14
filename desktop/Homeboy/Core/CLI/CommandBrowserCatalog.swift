import Foundation

enum CommandBrowserCatalog {
    static let annotatedCommands: [CommandBrowserEntry] = [
        .init(command: "project", summary: "Manage project/target configuration", scope: .project, risk: .guardedWrites, coverage: .workflow, workflow: .settings),
        .init(command: "ssh", summary: "Open SSH sessions for configured servers", scope: .server, risk: .operatorOnly, coverage: .partial, workflow: nil),
        .init(command: "server", summary: "Manage SSH server records", scope: .server, risk: .guardedWrites, coverage: .workflow, workflow: .settings),
        .init(command: "test", summary: "Run component tests", scope: .component, risk: .readOnly, coverage: .workflow, workflow: .quality),
        .init(command: "bench", summary: "Run performance benchmarks", scope: .component, risk: .readOnly, coverage: .workflow, workflow: .bench),
        .init(command: "trace", summary: "Capture behavioral traces", scope: .component, risk: .readOnly, coverage: .cliOnly, workflow: nil),
        .init(command: "observe", summary: "Persist passive observation evidence", scope: .global, risk: .readOnly, coverage: .cliOnly, workflow: nil),
        .init(command: "lint", summary: "Run component lint checks", scope: .component, risk: .readOnly, coverage: .workflow, workflow: .quality),
        .init(command: "db", summary: "Inspect and operate on databases", scope: .project, risk: .guardedWrites, coverage: .workflow, workflow: .databaseBrowser),
        .init(command: "deps", summary: "Manage component dependencies", scope: .component, risk: .mutating, coverage: .cliOnly, workflow: nil),
        .init(command: "doctor", summary: "Run local diagnostics", scope: .global, risk: .readOnly, coverage: .cliOnly, workflow: nil),
        .init(command: "file", summary: "Read and edit remote files", scope: .project, risk: .guardedWrites, coverage: .workflow, workflow: .remoteFileEditor),
        .init(command: "fleet", summary: "Manage groups of projects", scope: .project, risk: .guardedWrites, coverage: .partial, workflow: nil),
        .init(command: "logs", summary: "View remote logs", scope: .project, risk: .readOnly, coverage: .workflow, workflow: .remoteLogViewer),
        .init(command: "triage", summary: "Summarize project/component attention", scope: .global, risk: .readOnly, coverage: .workflow, workflow: .quality),
        .init(command: "deploy", summary: "Deploy components to targets", scope: .project, risk: .mutating, coverage: .workflow, workflow: .deployer),
        .init(command: "component", summary: "Manage standalone component records", scope: .component, risk: .guardedWrites, coverage: .workflow, workflow: .settings),
        .init(command: "config", summary: "Manage raw Homeboy configuration", scope: .global, risk: .mutating, coverage: .cliOnly, workflow: nil),
        .init(command: "daemon", summary: "Run the local HTTP API daemon", scope: .global, risk: .operatorOnly, coverage: .cliOnly, workflow: nil),
        .init(command: "extension", summary: "Install, setup, and run CLI extensions", scope: .global, risk: .guardedWrites, coverage: .workflow, workflow: .settings),
        .init(command: "status", summary: "Show actionable workspace status", scope: .global, risk: .readOnly, coverage: .workflow, workflow: nil),
        .init(command: "docs", summary: "Display CLI documentation", scope: .global, risk: .readOnly, coverage: .cliOnly, workflow: nil),
        .init(command: "changelog", summary: "Inspect generated changelog data", scope: .component, risk: .readOnly, coverage: .cliOnly, workflow: nil),
        .init(command: "git", summary: "Run git workflows for components", scope: .component, risk: .mutating, coverage: .readOnly, workflow: .git),
        .init(command: "issues", summary: "Reconcile findings with issue trackers", scope: .component, risk: .guardedWrites, coverage: .cliOnly, workflow: nil),
        .init(command: "version", summary: "Inspect or plan component versions", scope: .component, risk: .guardedWrites, coverage: .workflow, workflow: .release),
        .init(command: "build", summary: "Build a component", scope: .component, risk: .readOnly, coverage: .workflow, workflow: .release),
        .init(command: "changes", summary: "Show changes since the release baseline", scope: .component, risk: .readOnly, coverage: .workflow, workflow: .release),
        .init(command: "release", summary: "Plan or run release workflows", scope: .component, risk: .mutating, coverage: .readOnly, workflow: .release),
        .init(command: "review", summary: "Run scoped audit/lint/test review", scope: .component, risk: .readOnly, coverage: .workflow, workflow: .quality),
        .init(command: "audit", summary: "Detect architectural drift", scope: .component, risk: .readOnly, coverage: .workflow, workflow: .quality),
        .init(command: "refactor", summary: "Run structural refactoring helpers", scope: .component, risk: .mutating, coverage: .partial, workflow: nil),
        .init(command: "rig", summary: "Manage reproducible local dev environments", scope: .rig, risk: .mutating, coverage: .workflow, workflow: .rigs),
        .init(command: "runs", summary: "Inspect persisted run history and artifacts", scope: .global, risk: .readOnly, coverage: .workflow, workflow: .runHistory),
        .init(command: "self", summary: "Inspect the active Homeboy binary", scope: .global, risk: .readOnly, coverage: .cliOnly, workflow: nil),
        .init(command: "stack", summary: "Manage combined-fixes branch stacks", scope: .stack, risk: .mutating, coverage: .readOnly, workflow: .stackManager),
        .init(command: "undo", summary: "Undo recent Homeboy write operations", scope: .global, risk: .mutating, coverage: .partial, workflow: nil),
        .init(command: "auth", summary: "Authenticate with a project API", scope: .project, risk: .guardedWrites, coverage: .workflow, workflow: .apiAuth),
        .init(command: "api", summary: "Make API requests to a project", scope: .project, risk: .guardedWrites, coverage: .readOnly, workflow: .apiAuth),
        .init(command: "upgrade", summary: "Upgrade Homeboy CLI", scope: .global, risk: .mutating, coverage: .workflow, workflow: .settings),
        .init(command: "list", summary: "List available commands", scope: .global, risk: .readOnly, coverage: .cliOnly, workflow: nil),
        .init(command: "cargo", summary: "Run Cargo through Homeboy", scope: .component, risk: .operatorOnly, coverage: .cliOnly, workflow: nil),
        .init(command: "wp", summary: "Run WP-CLI against WordPress targets", scope: .project, risk: .operatorOnly, coverage: .cliOnly, workflow: nil)
    ]

    static func entries(discoveredCommands: [HomeboyCommandSummary]) -> [CommandBrowserEntry] {
        let metadata = Dictionary(uniqueKeysWithValues: annotatedCommands.map { ($0.command, $0) })
        return discoveredCommands.map { summary in
            (metadata[summary.command] ?? fallbackEntry(command: summary.command, summary: summary.summary))
                .withSummary(summary.summary)
        }
        .sorted { $0.command < $1.command }
    }

    private static func fallbackEntry(command: String, summary: String) -> CommandBrowserEntry {
        CommandBrowserEntry(
            command: command,
            summary: summary,
            scope: .global,
            risk: .operatorOnly,
            coverage: .cliOnly,
            workflow: nil
        )
    }
}
