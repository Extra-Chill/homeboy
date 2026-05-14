import AppKit
import Foundation

@MainActor
final class CommandBrowserViewModel: ObservableObject {
    @Published var commands = CommandBrowserCatalog.annotatedCommands
    @Published var selectedCommand: CommandBrowserEntry? = CommandBrowserCatalog.annotatedCommands.first
    @Published var filter = ""
    @Published var helpOutput = ""
    @Published var runOutput = ""
    @Published var commandInput = "homeboy --help"
    @Published var isLoadingHelp = false
    @Published var isRunning = false
    @Published var error: (any DisplayableError)?

    private let cli = HomeboyCLI.shared

    var filteredCommands: [CommandBrowserEntry] {
        let query = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !query.isEmpty else { return commands }
        return commands.filter { entry in
            entry.command.lowercased().contains(query)
                || entry.summary.lowercased().contains(query)
                || entry.scope.rawValue.lowercased().contains(query)
                || entry.coverage.rawValue.lowercased().contains(query)
        }
    }

    func select(_ command: CommandBrowserEntry) {
        selectedCommand = command
        commandInput = command.invocation + " --help"
        loadHelp(for: command)
    }

    func loadInitialHelp() {
        guard helpOutput.isEmpty else { return }

        Task {
            await refreshCatalog()
            if let selectedCommand {
                loadHelp(for: selectedCommand)
            }
        }
    }

    func loadHelp(for command: CommandBrowserEntry) {
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings -> CLI.", source: "Commands")
            return
        }

        isLoadingHelp = true
        error = nil

        Task {
            do {
                helpOutput = try await cli.commandHelp(for: command)
            } catch {
                self.error = error.toDisplayableError(source: "Commands")
                helpOutput = error.localizedDescription
            }

            isLoadingHelp = false
        }
    }

    func copyCurrentCommand() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(commandInput, forType: .string)
    }

    func runCommandInput() {
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings -> CLI.", source: "Commands")
            return
        }

        let invocation: HomeboyRawCommandInvocation
        do {
            invocation = try cli.parseRawInvocation(commandInput)
        } catch {
            self.error = error.toDisplayableError(source: "Commands")
            return
        }

        isRunning = true
        error = nil
        runOutput = "> \(invocation.displayCommand)\n"

        Task {
            do {
                let response = try await cli.runRawInvocation(invocation)
                appendRunOutput(response.output)
                appendRunOutput(response.errorOutput)
                appendRunOutput("exit \(response.exitCode)")
            } catch {
                self.error = error.toDisplayableError(source: "Commands")
                appendRunOutput(error.localizedDescription)
            }

            isRunning = false
        }
    }

    private func refreshCatalog() async {
        guard cli.isInstalled else { return }

        do {
            commands = try await cli.commandBrowserEntries()
            if let selectedCommand, commands.contains(where: { $0.id == selectedCommand.id }) {
                self.selectedCommand = commands.first { $0.id == selectedCommand.id }
            } else {
                selectedCommand = commands.first
                commandInput = selectedCommand.map { $0.invocation + " --help" } ?? "homeboy --help"
            }
        } catch {
            self.error = error.toDisplayableError(source: "Commands")
        }
    }

    private func appendRunOutput(_ text: String) {
        guard !text.isEmpty else { return }
        if !runOutput.isEmpty && !runOutput.hasSuffix("\n") {
            runOutput += "\n"
        }
        runOutput += text
        if !runOutput.hasSuffix("\n") {
            runOutput += "\n"
        }
    }
}
