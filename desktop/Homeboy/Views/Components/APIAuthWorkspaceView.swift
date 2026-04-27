import AppKit
import SwiftUI

struct APIAuthWorkspaceView: View {
    @ObservedObject private var config = ConfigurationManager.shared

    @State private var selectedProjectId: String = ""
    @State private var authStatus: HomeboyAuthOutput?
    @State private var endpoint = "/wp/v2/types"
    @State private var identifier = ""
    @State private var password = ""
    @State private var apiResult: HomeboyAPIGetOutput?
    @State private var isLoadingStatus = false
    @State private var isRunningRequest = false
    @State private var isLoggingIn = false
    @State private var errorMessage: String?

    private var projects: [ProjectConfiguration] {
        config.availableProjects.isEmpty ? [config.safeActiveProject] : config.availableProjects
    }

    private var selectedProject: ProjectConfiguration? {
        projects.first { $0.id == selectedProjectId } ?? config.activeProject
    }

    private var selectedProjectAPIConfig: APIConfig? {
        selectedProject?.api
    }

    private var canRunGet: Bool {
        !selectedProjectId.isEmpty && !endpoint.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !isRunningRequest
    }

    private var canLogin: Bool {
        !selectedProjectId.isEmpty && !identifier.isEmpty && !password.isEmpty && !isLoggingIn
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    projectPicker
                    authPanel
                    getExplorer
                }
                .padding(24)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .task {
            initializeSelection()
            await refreshStatus()
        }
        .onChange(of: config.activeProject?.id) { _, _ in
            initializeSelection()
            Task { await refreshStatus() }
        }
    }

    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text("API/Auth")
                    .font(.largeTitle)
                    .fontWeight(.semibold)
                Text("Homeboy project API authentication and safe read-only requests")
                    .foregroundColor(.secondary)
            }

            Spacer()

            Button {
                Task { await refreshStatus() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(isLoadingStatus || selectedProjectId.isEmpty)
        }
        .padding(24)
    }

    private var projectPicker: some View {
        GroupBox("Project") {
            VStack(alignment: .leading, spacing: 12) {
                Picker("Project", selection: $selectedProjectId) {
                    ForEach(projects) { project in
                        Text(project.id).tag(project.id)
                    }
                }
                .pickerStyle(.menu)
                .onChange(of: selectedProjectId) { _, _ in
                    authStatus = nil
                    apiResult = nil
                    errorMessage = nil
                    Task { await refreshStatus() }
                }

                if let api = selectedProjectAPIConfig {
                    LabeledContent("API enabled") {
                        Text(api.enabled ? "Yes" : "No")
                            .foregroundStyle(api.enabled ? .green : .secondary)
                    }
                    LabeledContent("Base URL") {
                        Text(api.baseURL.isEmpty ? "Not configured" : api.baseURL)
                            .textSelection(.enabled)
                            .foregroundStyle(api.baseURL.isEmpty ? .secondary : .primary)
                    }
                }

                if let errorMessage {
                    InlineErrorView(errorMessage, source: "API/Auth")
                }
            }
            .frame(maxWidth: 640, alignment: .leading)
        }
    }

    private var authPanel: some View {
        GroupBox("Project API Auth") {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 12) {
                    statusBadge
                    Spacer()
                    Button("Check Status") {
                        Task { await refreshStatus() }
                    }
                    .disabled(isLoadingStatus || selectedProjectId.isEmpty)

                    Button("Logout") {
                        Task { await logout() }
                    }
                    .disabled(selectedProjectId.isEmpty || isLoadingStatus)
                }

                Text("This uses `homeboy auth`, which stores credentials in Homeboy's project-scoped auth workspace. It does not use the desktop app login session.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                VStack(alignment: .leading, spacing: 10) {
                    TextField("Username or email", text: $identifier)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 360)
                    SecureField("Password", text: $password)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 360)
                    Button {
                        Task { await login() }
                    } label: {
                        if isLoggingIn {
                            ProgressView()
                                .controlSize(.small)
                        } else {
                            Label("Login with Homeboy", systemImage: "key")
                        }
                    }
                    .disabled(!canLogin)
                }
            }
            .frame(maxWidth: 640, alignment: .leading)
        }
    }

    private var statusBadge: some View {
        HStack(spacing: 8) {
            if isLoadingStatus {
                ProgressView()
                    .controlSize(.small)
                Text("Checking auth status")
            } else if authStatus?.authenticated == true {
                Image(systemName: "checkmark.seal.fill")
                    .foregroundStyle(.green)
                Text("Authenticated")
            } else if authStatus != nil {
                Image(systemName: "person.crop.circle.badge.xmark")
                    .foregroundStyle(.secondary)
                Text("Not authenticated")
            } else {
                Image(systemName: "questionmark.circle")
                    .foregroundStyle(.secondary)
                Text("Status unknown")
            }
        }
    }

    private var getExplorer: some View {
        GroupBox("Safe GET Explorer") {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    TextField("Endpoint", text: $endpoint)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(.body, design: .monospaced))
                    Button {
                        Task { await runGet() }
                    } label: {
                        if isRunningRequest {
                            ProgressView()
                                .controlSize(.small)
                        } else {
                            Label("Run GET", systemImage: "play.fill")
                        }
                    }
                    .disabled(!canRunGet)
                }

                Text("Only `homeboy api <project> get <endpoint>` is exposed here. Mutating verbs stay out of the desktop UI until preview and confirmation are designed.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let apiResult {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("Response")
                                .font(.headline)
                            Spacer()
                            Button("Copy JSON") {
                                copyToClipboard(apiResult.response.prettyPrintedJSONString)
                            }
                        }
                        Text(apiResult.response.prettyPrintedJSONString)
                            .font(.system(.body, design: .monospaced))
                            .textSelection(.enabled)
                            .padding(12)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color(nsColor: .textBackgroundColor))
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                    }
                } else {
                    ContentUnavailableView(
                        "No API Response",
                        systemImage: "curlybraces",
                        description: Text("Run a GET request to inspect the project's configured API.")
                    )
                    .frame(maxWidth: .infinity, minHeight: 180)
                }
            }
        }
    }

    private func initializeSelection() {
        if selectedProjectId.isEmpty || !projects.contains(where: { $0.id == selectedProjectId }) {
            selectedProjectId = config.activeProject?.id ?? projects.first?.id ?? ""
        }
    }

    private func refreshStatus() async {
        guard !selectedProjectId.isEmpty else { return }
        isLoadingStatus = true
        errorMessage = nil
        defer { isLoadingStatus = false }

        do {
            authStatus = try await HomeboyCLI.shared.authStatus(projectId: selectedProjectId)
        } catch {
            authStatus = nil
            errorMessage = error.localizedDescription
        }
    }

    private func login() async {
        guard canLogin else { return }
        isLoggingIn = true
        errorMessage = nil
        defer { isLoggingIn = false }

        do {
            authStatus = try await HomeboyCLI.shared.authLogin(
                projectId: selectedProjectId,
                identifier: identifier,
                password: password
            )
            password = ""
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func logout() async {
        guard !selectedProjectId.isEmpty else { return }
        isLoadingStatus = true
        errorMessage = nil
        defer { isLoadingStatus = false }

        do {
            _ = try await HomeboyCLI.shared.authLogout(projectId: selectedProjectId)
            authStatus = try? await HomeboyCLI.shared.authStatus(projectId: selectedProjectId)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func runGet() async {
        guard canRunGet else { return }
        isRunningRequest = true
        errorMessage = nil
        apiResult = nil
        defer { isRunningRequest = false }

        let normalizedEndpoint = endpoint.hasPrefix("/") ? endpoint : "/\(endpoint)"

        do {
            apiResult = try await HomeboyCLI.shared.apiGet(projectId: selectedProjectId, endpoint: normalizedEndpoint)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func copyToClipboard(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }
}

#Preview {
    APIAuthWorkspaceView()
}
