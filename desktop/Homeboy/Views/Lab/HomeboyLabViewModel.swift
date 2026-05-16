import AppKit
import Foundation

@MainActor
final class HomeboyLabViewModel: ObservableObject {
    @Published var runners: [HomeboyRunner] = [HomeboyRunner.localDefault]
    @Published var selectedRunnerID = HomeboyRunner.localDefault.id
    @Published var doctorReport: RunnerDoctorOutput?
    @Published var connection: RunnerConnection?
    @Published var jobs: [DaemonJob] = []
    @Published var selectedJobID: String?
    @Published var selectedJob: DaemonJob?
    @Published var events: [DaemonJobEvent] = []
    @Published var artifacts: [RunArtifactSyncItem] = []
    @Published var components: [DeployableComponent] = []
    @Published var selectedComponentID: String?
    @Published var selectedAction = "audit"
    @Published var extraArguments = ""
    @Published var isLoadingRunners = false
    @Published var isRunningDoctor = false
    @Published var isConnecting = false
    @Published var isLoadingJobs = false
    @Published var isStartingJob = false
    @Published var isLoadingArtifacts = false
    @Published var error: (any DisplayableError)?
    @Published var warning: AppWarning?

    let actions = ["audit", "lint", "test", "bench"]

    private let cli = HomeboyCLI.shared
    private var pollingTask: Task<Void, Never>?

    var selectedRunner: HomeboyRunner? {
        runners.first { $0.id == selectedRunnerID }
    }

    var selectedComponent: DeployableComponent? {
        components.first { $0.id == selectedComponentID }
    }

    var isLocalSelected: Bool {
        selectedRunnerID == HomeboyRunner.localDefault.id || selectedRunner?.kind == "local"
    }

    var isConnected: Bool {
        connection?.connected == true
    }

    var daemonURL: String? {
        connection?.localUrl ?? connection?.session?.localUrl
    }

    var canStartRemoteJob: Bool {
        !isLocalSelected && isConnected && selectedComponent != nil && !isStartingJob
    }

    init() {
        loadComponents()
    }

    deinit {
        pollingTask?.cancel()
    }

    func loadInitialState() {
        loadComponents()
        loadRunners()
    }

    func loadComponents() {
        let project = ConfigurationManager.shared.activeProject ?? ConfigurationManager.shared.safeActiveProject
        components = ConfigurationManager.shared.loadComponentsForProject(project).map { DeployableComponent(from: $0) }
        if selectedComponentID == nil || !components.contains(where: { $0.id == selectedComponentID }) {
            selectedComponentID = components.first?.id
        }
    }

    func loadRunners() {
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings -> CLI.", source: "Homeboy Lab")
            return
        }

        isLoadingRunners = true
        error = nil
        warning = nil

        Task {
            do {
                runners = try await cli.runnerList()
                if !runners.contains(where: { $0.id == selectedRunnerID }) {
                    selectedRunnerID = HomeboyRunner.localDefault.id
                }
                await refreshSelectedRunner()
            } catch {
                runners = [HomeboyRunner.localDefault]
                selectedRunnerID = HomeboyRunner.localDefault.id
                warning = AppWarning(
                    "This Homeboy CLI does not expose the Wave 1 runner registry yet. Local execution remains available; install a Homeboy build containing Extra-Chill/homeboy#2533, #2534, and #2536 to use Lab runners.",
                    source: "Homeboy Lab"
                )
            }
            isLoadingRunners = false
        }
    }

    func refreshSelectedRunner() async {
        doctorReport = nil
        connection = nil
        jobs = []
        selectedJobID = nil
        selectedJob = nil
        events = []
        artifacts = []
        await runDoctor()
        await loadRunnerStatus()
        await refreshJobs()
    }

    func selectedRunnerDidChange() {
        pollingTask?.cancel()
        Task { await refreshSelectedRunner() }
    }

    func runDoctor() async {
        guard cli.isInstalled else { return }
        isRunningDoctor = true
        error = nil
        do {
            doctorReport = try await cli.runnerDoctor(id: selectedRunnerID)
        } catch {
            self.error = error.toDisplayableError(source: "Homeboy Lab")
        }
        isRunningDoctor = false
    }

    func connectSelectedRunner() {
        guard !isLocalSelected else { return }
        isConnecting = true
        error = nil
        Task {
            do {
                connection = try await cli.runnerConnect(id: selectedRunnerID)
                await refreshJobs()
            } catch {
                self.error = error.toDisplayableError(source: "Homeboy Lab")
            }
            isConnecting = false
        }
    }

    func disconnectSelectedRunner() {
        guard !isLocalSelected else { return }
        isConnecting = true
        error = nil
        Task {
            do {
                connection = try await cli.runnerDisconnect(id: selectedRunnerID)
                pollingTask?.cancel()
                jobs = []
                selectedJobID = nil
                selectedJob = nil
                events = []
            } catch {
                self.error = error.toDisplayableError(source: "Homeboy Lab")
            }
            isConnecting = false
        }
    }

    func loadRunnerStatus() async {
        guard !isLocalSelected else { return }
        do {
            connection = try await cli.runnerStatus(id: selectedRunnerID)
        } catch {
            warning = AppWarning("Runner status is unavailable: \(error.localizedDescription)", source: "Homeboy Lab")
        }
    }

    func startRemoteJob() {
        guard canStartRemoteJob, let component = selectedComponent else { return }
        isStartingJob = true
        error = nil
        Task {
            do {
                let client = try daemonClient()
                var body: [String: JSONValue] = ["component": .string(component.id)]
                if !component.localPath.isEmpty {
                    body["path"] = .string(component.localPath)
                }
                let args = ShellCommandLineParser.arguments(from: extraArguments)
                if !args.isEmpty {
                    body["args"] = .array(args.map(JSONValue.string))
                }
                let output = try await client.enqueue(kind: selectedAction, body: body)
                selectedJobID = output.job.id
                selectedJob = output.job
                await refreshJobs()
                startPollingSelectedJob()
            } catch {
                self.error = error.toDisplayableError(source: "Homeboy Lab")
            }
            isStartingJob = false
        }
    }

    func refreshJobs() async {
        guard !isLocalSelected, isConnected else { return }
        isLoadingJobs = true
        do {
            let client = try daemonClient()
            jobs = try await client.listJobs().sorted { $0.updatedAtMs > $1.updatedAtMs }
            if let selectedJobID, jobs.contains(where: { $0.id == selectedJobID }) {
                await selectJob(selectedJobID)
            } else if let first = jobs.first {
                selectedJobID = first.id
                await selectJob(first.id)
            }
        } catch {
            self.error = error.toDisplayableError(source: "Homeboy Lab")
        }
        isLoadingJobs = false
    }

    func selectJob(_ id: String?) async {
        selectedJobID = id
        events = []
        artifacts = []
        guard let id, !isLocalSelected, isConnected else {
            selectedJob = nil
            return
        }

        do {
            let client = try daemonClient()
            async let jobTask = client.showJob(id: id)
            async let eventsTask = client.jobEvents(id: id)
            selectedJob = try await jobTask
            events = try await eventsTask
            loadArtifactsFromResultEvents()
            startPollingSelectedJob()
        } catch {
            self.error = error.toDisplayableError(source: "Homeboy Lab")
        }
    }

    func cancelSelectedJob() {
        guard let id = selectedJobID else { return }
        Task {
            do {
                let client = try daemonClient()
                selectedJob = try await client.cancelJob(id: id)
                await refreshJobs()
            } catch {
                self.error = error.toDisplayableError(source: "Homeboy Lab")
            }
        }
    }

    func downloadAndOpenArtifact(_ artifact: RunArtifactSyncItem) {
        isLoadingArtifacts = true
        Task {
            do {
                let client = try daemonClient()
                let url = try await client.downloadArtifact(runId: artifact.runId, artifactId: artifact.id)
                NSWorkspace.shared.open(url)
            } catch {
                self.error = error.toDisplayableError(source: "Homeboy Lab")
            }
            isLoadingArtifacts = false
        }
    }

    private func loadArtifactsFromResultEvents() {
        let runIds = events.compactMap { event -> String? in
            guard event.kind == "result", let data = event.data else { return nil }
            return firstRunId(in: data)
        }
        guard let runId = runIds.first else { return }

        isLoadingArtifacts = true
        Task {
            do {
                artifacts = try await daemonClient().artifactSyncManifest(runId: runId)
            } catch {
                warning = AppWarning("Job completed, but artifacts are not available yet: \(error.localizedDescription)", source: "Homeboy Lab")
            }
            isLoadingArtifacts = false
        }
    }

    private func startPollingSelectedJob() {
        pollingTask?.cancel()
        guard let id = selectedJobID else { return }

        pollingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                guard let self else { return }
                if await self.pollJob(id) {
                    return
                }
            }
        }
    }

    private func pollJob(_ id: String) async -> Bool {
        do {
            let client = try daemonClient()
            selectedJob = try await client.showJob(id: id)
            events = try await client.jobEvents(id: id)
            loadArtifactsFromResultEvents()
            return selectedJob.map { ["succeeded", "failed", "cancelled"].contains($0.status) } ?? true
        } catch {
            return true
        }
    }

    private func daemonClient() throws -> HomeboyDaemonClient {
        guard let daemonURL else {
            throw HomeboyDaemonError(statusCode: 0, body: "Runner is not connected to a Homeboy daemon")
        }
        return try HomeboyDaemonClient(baseURL: daemonURL)
    }

    private func firstRunId(in value: JSONValue) -> String? {
        switch value {
        case .object(let object):
            if let runId = object["run_id"]?.stringValue, !runId.isEmpty, runId != "null" {
                return runId
            }
            if let runId = object["runId"]?.stringValue, !runId.isEmpty, runId != "null" {
                return runId
            }
            return object.values.compactMap(firstRunId).first
        case .array(let values):
            return values.compactMap(firstRunId).first
        default:
            return nil
        }
    }
}
