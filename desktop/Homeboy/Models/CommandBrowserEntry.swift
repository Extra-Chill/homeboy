import Foundation

struct CommandBrowserEntry: Identifiable, Hashable {
    enum Scope: String {
        case global = "Global"
        case project = "Project"
        case component = "Component"
        case rig = "Rig"
        case stack = "Stack"
        case server = "Server"
    }

    enum Risk: String {
        case readOnly = "Read-only"
        case guardedWrites = "Guarded writes"
        case mutating = "Mutating"
        case operatorOnly = "Operator"
    }

    enum DesktopCoverage: String {
        case workflow = "Workflow UI"
        case partial = "Partial UI"
        case readOnly = "Read-only UI"
        case cliOnly = "CLI-only"
    }

    let command: String
    let summary: String
    let scope: Scope
    let risk: Risk
    let coverage: DesktopCoverage
    let workflow: CoreTool?

    var id: String { command }
    var invocation: String { "homeboy \(command)" }

    func withSummary(_ summary: String) -> CommandBrowserEntry {
        CommandBrowserEntry(
            command: command,
            summary: summary,
            scope: scope,
            risk: risk,
            coverage: coverage,
            workflow: workflow
        )
    }
}
