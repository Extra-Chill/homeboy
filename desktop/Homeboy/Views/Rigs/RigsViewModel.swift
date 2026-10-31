import Foundation

@MainActor
final class RigsViewModel: ObservableObject {
    enum MutatingAction: String, Identifiable {
        case up = "up"
        case down = "down"

        var id: String { rawValue }

        var title: String {
            switch self {
            case .up: return "Run Rig Up"
            case .down: return "Run Rig Down"
            }
        }
    }

    @Published var rigs: [RigListItem] = []
    @Published var selectedRigID: String?
    @Published var selectedRig: RigSpec?
    @Published var status: RigStatusOutput?
    @Published var checkResult: RigCheckOutput?
    @Published var consoleOutput = ""
    @Published var isLoading = false
    @Published var runningCommand: String?
    @Published var error: (any DisplayableError)?
    @Published var pendingMutatingAction: MutatingAction?

    var selectedRigListItem: RigListItem? {
        rigs.first { $0.id == selectedRigID }
    }

    var isRunningCommand: Bool {
        runningCommand != nil
    }

    func load() {
        guard HomeboyCLI.shared.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Rigs")
            return
        }

        isLoading = true
        error = nil

        Task {
            do {
                let loaded = try await HomeboyCLI.shared.rigList()
                rigs = loaded
                isLoading = false

                if selectedRigID == nil || !loaded.contains(where: { $0.id == selectedRigID }) {
                    selectedRigID = loaded.first?.id
                }

                if let id = selectedRigID {
                    await loadRig(id: id)
                }
            } catch {
                isLoading = false
                self.error = error.toDisplayableError(source: "Rigs")
                appendConsole("> Error: \(error.localizedDescription)")
            }
        }
    }

    func selectRig(_ id: String) {
        guard selectedRigID != id else { return }
        selectedRigID = id
        selectedRig = nil
        status = nil
        checkResult = nil

        Task {
            await loadRig(id: id)
        }
    }

    func refreshSelectedRig() {
        guard let id = selectedRigID else { return }
        Task {
            await loadRig(id: id)
        }
    }

    func runStatus() {
        guard let id = selectedRigID else { return }
        runReadOnlyCommand(name: "status", id: id) {
            let output = try await HomeboyCLI.shared.rigStatus(id: id)
            self.status = output
            self.appendConsole("\(output.services.count) service(s) reported")
        }
    }

    func runCheck() {
        guard let id = selectedRigID else { return }
        runCommand(name: "check", id: id) {
            let result = try await HomeboyCLI.shared.rigCheck(id: id)
            self.checkResult = result.data
            self.appendConsole(result.rawOutput)

            if !result.errorOutput.isEmpty {
                self.appendConsole(result.errorOutput)
            }
        }
    }

    func confirmMutatingAction(_ action: MutatingAction) {
        pendingMutatingAction = action
    }

    func runPendingMutatingAction() {
        guard let action = pendingMutatingAction, let id = selectedRigID else { return }
        pendingMutatingAction = nil

        runCommand(name: action.rawValue, id: id) {
            let result: RigCommandResult<RigLifecycleOutput>
            switch action {
            case .up:
                result = try await HomeboyCLI.shared.rigUp(id: id)
            case .down:
                result = try await HomeboyCLI.shared.rigDown(id: id)
            }

            self.appendConsole(result.rawOutput)

            if !result.errorOutput.isEmpty {
                self.appendConsole(result.errorOutput)
            }

            await self.loadRig(id: id)
        }
    }

    func clearConsole() {
        consoleOutput = ""
    }

    private func loadRig(id: String) async {
        do {
            async let specTask = HomeboyCLI.shared.rigShow(id: id)
            async let statusTask = HomeboyCLI.shared.rigStatus(id: id)
            selectedRig = try await specTask
            status = try await statusTask
        } catch {
            self.error = error.toDisplayableError(source: "Rigs")
            appendConsole("> Error loading \(id): \(error.localizedDescription)")
        }
    }

    private func runReadOnlyCommand(name: String, id: String, operation: @escaping () async throws -> Void) {
        runCommand(name: name, id: id, operation: operation)
    }

    private func runCommand(name: String, id: String, operation: @escaping () async throws -> Void) {
        guard runningCommand == nil else { return }

        runningCommand = name
        error = nil
        appendConsole("> homeboy rig \(name) \(id)")

        Task {
            do {
                try await operation()
            } catch {
                self.error = error.toDisplayableError(source: "Rigs")
                appendConsole("> Error: \(error.localizedDescription)")
            }

            runningCommand = nil
        }
    }

    private func appendConsole(_ text: String) {
        guard !text.isEmpty else { return }
        if !consoleOutput.isEmpty && !consoleOutput.hasSuffix("\n") {
            consoleOutput += "\n"
        }
        consoleOutput += text
        if !consoleOutput.hasSuffix("\n") {
            consoleOutput += "\n"
        }
    }
}
