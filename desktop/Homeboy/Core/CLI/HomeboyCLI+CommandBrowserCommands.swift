import Foundation

struct HomeboyCommandSummary: Hashable {
    let command: String
    let summary: String
}

struct HomeboyRawCommandInvocation: Hashable {
    let displayCommand: String
    let args: [String]
}

@MainActor
extension HomeboyCLI {
    func commandBrowserEntries() async throws -> [CommandBrowserEntry] {
        let response = try await cli.execute(["--help"], timeout: 30)
        let discovered = Self.commandSummaries(fromHelp: response.output)
        guard !discovered.isEmpty else { return CommandBrowserCatalog.annotatedCommands }
        return CommandBrowserCatalog.entries(discoveredCommands: discovered)
    }

    func commandHelp(for command: CommandBrowserEntry) async throws -> String {
        let response = try await cli.execute([command.command, "--help"], timeout: 30)
        return response.output.isEmpty ? response.errorOutput : response.output
    }

    func parseRawInvocation(_ input: String) throws -> HomeboyRawCommandInvocation {
        let parsed = ShellCommandLineParser.arguments(from: input)
        guard let first = parsed.first, first == "homeboy" else {
            throw CLIBridgeError.invalidResponse("Commands must start with `homeboy`.")
        }

        return HomeboyRawCommandInvocation(displayCommand: input, args: Array(parsed.dropFirst()))
    }

    func runRawInvocation(_ invocation: HomeboyRawCommandInvocation) async throws -> CLIBridgeResponse {
        try await cli.execute(invocation.args, timeout: 300)
    }

    private static func commandSummaries(fromHelp output: String) -> [HomeboyCommandSummary] {
        var isInCommands = false
        var commands: [HomeboyCommandSummary] = []

        for line in output.components(separatedBy: .newlines) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed == "Commands:" {
                isInCommands = true
                continue
            }
            if trimmed == "Options:" {
                break
            }
            guard isInCommands, !trimmed.isEmpty, !trimmed.hasPrefix("-") else { continue }

            let parts = trimmed.split(maxSplits: 1, whereSeparator: { $0.isWhitespace })
            guard let command = parts.first else { continue }
            commands.append(HomeboyCommandSummary(
                command: String(command),
                summary: parts.count > 1 ? String(parts[1]) : ""
            ))
        }

        return commands
    }
}
