import SwiftUI

/// Fleet management view - list and manage fleets
struct FleetManagementView: View {
    @State private var fleets: [Fleet] = []
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var showingCreateSheet = false
    @State private var selectedFleet: Fleet?

    var body: some View {
        NavigationSplitView {
            fleetList
        } detail: {
            if let fleet = selectedFleet {
                FleetDetailView(fleet: fleet)
            } else {
                ContentUnavailableView(
                    "Select a Fleet",
                    systemImage: "shippingbox",
                    description: Text("Choose a fleet from the sidebar to view details")
                )
            }
        }
        .sheet(isPresented: $showingCreateSheet) {
            FleetCreateSheet { newFleet in
                fleets.append(newFleet)
            }
        }
    }

    private var fleetList: some View {
        List(selection: $selectedFleet) {
            Section("Fleets") {
                ForEach(fleets) { fleet in
                    NavigationLink(value: fleet) {
                        FleetRow(fleet: fleet)
                    }
                }
            }
        }
        .navigationTitle("Fleets")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    showingCreateSheet = true
                } label: {
                    Label("New Fleet", systemImage: "plus")
                }
            }
            ToolbarItem(placement: .cancellationAction) {
                Button {
                    Task { await loadFleets() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(isLoading)
            }
        }
        .task {
            await loadFleets()
        }
    }

    private func loadFleets() async {
        isLoading = true
        errorMessage = nil
        do {
            fleets = try await HomeboyCLI.shared.fleetList()
        } catch {
            errorMessage = error.localizedDescription
        }
        isLoading = false
    }
}

/// Single fleet row in the list
struct FleetRow: View {
    let fleet: Fleet

    var body: some View {
        HStack {
            Image(systemName: "shippingbox.fill")
                .foregroundColor(.blue)
            VStack(alignment: .leading) {
                Text(fleet.id)
                    .font(.headline)
                if let desc = fleet.description {
                    Text(desc)
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            Spacer()
            Text("\(fleet.projectIds.count) project\(fleet.projectIds.count == 1 ? "" : "s")")
                .font(.caption)
                .foregroundColor(.secondary)
        }
    }
}

/// Fleet detail view - shows projects, components, status
struct FleetDetailView: View {
    let fleet: Fleet
    @State private var projects: [ProjectListItem] = []
    @State private var components: [String: [String]] = [:]
    @State private var status: FleetStatusOutput?
    @State private var isLoading = false

    var body: some View {
        VStack(alignment: .leading) {
            // Header
            HStack {
                VStack(alignment: .leading) {
                    Text(fleet.id)
                        .font(.title)
                    if let desc = fleet.description {
                        Text(desc)
                            .font(.subheadline)
                            .foregroundColor(.secondary)
                    }
                }
                Spacer()
                Button {
                    Task { await loadFleetData() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
            }
            .padding()

            Divider()

            // Content
            if isLoading {
                ProgressView("Loading fleet data...")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                fleetContent
            }
        }
        .task {
            await loadFleetData()
        }
    }

    private var fleetContent: some View {
        HStack(spacing: 0) {
            // Projects list
            VStack(alignment: .leading) {
                Text("Projects (\(projects.count))")
                    .font(.headline)
                    .padding()

                List(projects) { project in
                    HStack {
                        Image(systemName: "folder")
                            .foregroundColor(.blue)
                        Text(project.id)
                        Spacer()
                    }
                }
            }
            .frame(width: 250)

            Divider()

            // Components
            VStack(alignment: .leading) {
                Text("Components (\(components.count))")
                    .font(.headline)
                    .padding()

                ScrollView {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(components.keys.sorted()), id: \.self) { componentId in
                            HStack {
                                Image(systemName: "cube")
                                    .foregroundColor(.orange)
                                Text(componentId)
                                    .font(.subheadline)
                                Spacer()
                                Text("\(components[componentId]?.count ?? 0) projects")
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                            }
                            .padding(.horizontal)
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func loadFleetData() async {
        isLoading = true
        do {
            async let projectsTask = HomeboyCLI.shared.fleetProjects(fleetId: fleet.id)
            async let componentsTask = HomeboyCLI.shared.fleetComponents(fleetId: fleet.id)
            async let statusTask = HomeboyCLI.shared.fleetStatus(fleetId: fleet.id)

            projects = try await projectsTask
            components = try await componentsTask
            status = try await statusTask
        } catch {
            print("Failed to load fleet data: \(error)")
        }
        isLoading = false
    }
}

/// Sheet for creating a new fleet
struct FleetCreateSheet: View {
    @Environment(\.dismiss) private var dismiss
    let onCreate: (Fleet) -> Void

    @State private var id = ""
    @State private var description = ""
    @State private var projectIds = ""
    @State private var isCreating = false
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack {
            Form {
                Section("Fleet Details") {
                    TextField("ID", text: $id)
                        .textInputAutocapitalization(.never)
                    TextField("Description (optional)", text: $description)
                }

                Section("Projects") {
                    TextField("Project IDs (comma-separated)", text: $projectIds)
                        .textInputAutocapitalization(.never)
                }

                if let error = errorMessage {
                    Section {
                        Text(error)
                            .foregroundColor(.red)
                    }
                }
            }
            .navigationTitle("New Fleet")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") {
                        Task { await createFleet() }
                    }
                    .disabled(id.isEmpty || isCreating)
                }
            }
        }
        .frame(minWidth: 400, minHeight: 300)
    }

    private func createFleet() async {
        isCreating = true
        errorMessage = nil

        let ids = projectIds
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }

        do {
            let fleet = try await HomeboyCLI.shared.fleetCreate(
                id: id,
                description: description.isEmpty ? nil : description,
                projectIds: ids
            )
            onCreate(fleet)
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
        }

        isCreating = false
    }
}

// MARK: - Preview

#Preview("Fleet Management") {
    FleetManagementView()
        .frame(width: 1000, height: 600)
}
