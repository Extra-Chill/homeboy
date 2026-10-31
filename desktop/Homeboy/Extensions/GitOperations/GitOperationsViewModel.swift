import AppKit
import Combine
import Foundation

struct GitComponentState: Identifiable {
    let component: ComponentConfiguration
    var status: GitStatusOutput?
    var remoteURL: URL?
    var error: AppError?

    var id: String { component.id }

    var displayName: String { component.displayName }

    var path: String { component.localPath }

    var stateLabel: String {
        if let error { return error.body }
        guard let status else { return "Not loaded" }
        guard status.success else { return "Status failed" }
        return status.stdout.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "Clean" : "Changes"
    }

    var stateIcon: String {
        if error != nil { return "exclamationmark.triangle.fill" }
        guard let status else { return "circle" }
        guard status.success else { return "xmark.circle.fill" }
        return status.stdout.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "checkmark.circle.fill" : "circle.fill"
    }

    var stateColor: NSColor {
        if error != nil { return .systemOrange }
        guard let status else { return .secondaryLabelColor }
        guard status.success else { return .systemRed }
        return status.stdout.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? .systemGreen : .systemOrange
    }

    var githubIssuesURL: URL? {
        remoteURL?.appendingPathComponent("issues")
    }

    var githubPullRequestsURL: URL? {
        remoteURL?.appendingPathComponent("pulls")
    }
}

@MainActor
final class GitOperationsViewModel: ObservableObject, ConfigurationObserving {
    @Published var components: [GitComponentState] = []
    @Published var selectedComponentId: String?
    @Published var isLoading = false
    @Published var error: AppError?

    var cancellables = Set<AnyCancellable>()

    private let cli = HomeboyCLI.shared

    init() {
        loadComponents()
        observeConfiguration()
    }

    var selectedComponent: GitComponentState? {
        guard let selectedComponentId else { return components.first }
        return components.first { $0.id == selectedComponentId }
    }

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch, .projectModified:
            loadComponents()
        default:
            break
        }
    }

    func refresh() {
        guard HomeboyCLI.shared.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Git")
            return
        }

        isLoading = true
        error = nil

        Task {
            var refreshed: [GitComponentState] = []

            for state in components {
                var next = state

                do {
                    async let status = cli.gitStatus(componentId: state.id, path: state.path)
                    async let remote = githubRemoteURL(path: state.path)
                    next.status = try await status
                    next.remoteURL = await remote
                    next.error = nil
                } catch {
                    next.error = AppError(error.localizedDescription, source: "Git Status: \(state.id)")
                }

                refreshed.append(next)
            }

            components = refreshed
            isLoading = false
        }
    }

    func open(_ url: URL?) {
        guard let url else { return }
        NSWorkspace.shared.open(url)
    }

    private func loadComponents() {
        let project = ConfigurationManager.shared.safeActiveProject
        let loaded = ConfigurationManager.shared.loadComponentsForProject(project)
        components = loaded.map { GitComponentState(component: $0, status: nil, remoteURL: nil, error: nil) }

        if selectedComponentId == nil || !components.contains(where: { $0.id == selectedComponentId }) {
            selectedComponentId = components.first?.id
        }
    }

    private nonisolated func githubRemoteURL(path: String) async -> URL? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["git", "-C", path, "remote", "get-url", "origin"]

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe()

        do {
            try process.run()
            process.waitUntilExit()
        } catch {
            return nil
        }

        guard process.terminationStatus == 0 else { return nil }

        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        guard let remote = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines),
              !remote.isEmpty else {
            return nil
        }

        return normalizeGitHubURL(remote)
    }

    private nonisolated func normalizeGitHubURL(_ remote: String) -> URL? {
        if remote.hasPrefix("git@github.com:") {
            var path = remote.replacingOccurrences(of: "git@github.com:", with: "")
            if path.hasSuffix(".git") {
                path.removeLast(4)
            }
            return URL(string: "https://github.com/\(path)")
        }

        if remote.hasPrefix("https://github.com/") {
            var url = remote
            if url.hasSuffix(".git") {
                url.removeLast(4)
            }
            return URL(string: url)
        }

        return nil
    }
}
