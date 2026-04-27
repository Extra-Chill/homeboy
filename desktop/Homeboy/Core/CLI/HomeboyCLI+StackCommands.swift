import Foundation

@MainActor
extension HomeboyCLI {
    // MARK: - Stack Commands

    func stackList() async throws -> [StackListItem] {
        let output: StackListOutput = try await cli.executeCommand(
            ["stack", "list"],
            dataType: StackListOutput.self,
            source: "Stack List",
            timeout: 30
        )
        return output.stacks ?? []
    }

    func stackShow(id: String) async throws -> StackSpec {
        let output: StackShowOutput = try await cli.executeCommand(
            ["stack", "show", id],
            dataType: StackShowOutput.self,
            source: "Stack Show",
            timeout: 30
        )
        guard let stack = output.stack else {
            throw CLIBridgeError.invalidResponse("Stack not found: \(id)")
        }
        return stack
    }

    func stackStatus(id: String) async throws -> StackStatusOutput {
        try await cli.executeCommand(
            ["stack", "status", id],
            dataType: StackStatusOutput.self,
            source: "Stack Status",
            timeout: 120
        )
    }

    func stackInspect(path: String? = nil, base: String? = nil, includePRs: Bool = true) async throws -> StackInspectOutput {
        var args = ["stack", "inspect"]
        if let path {
            args.append(contentsOf: ["--path", path])
        }
        if let base {
            args.append(contentsOf: ["--base", base])
        }
        if !includePRs {
            args.append("--no-pr")
        }
        return try await cli.executeCommand(
            args,
            dataType: StackInspectOutput.self,
            source: "Stack Inspect",
            timeout: 120
        )
    }
}
