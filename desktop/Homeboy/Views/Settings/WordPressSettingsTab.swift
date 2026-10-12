import SwiftUI

struct WordPressSettingsTab: View {
    @ObservedObject var config: ConfigurationManager

    var body: some View {
        Form {
            Section("Database") {
                TextField("Table Prefix", text: Binding(
                    get: { config.safeActiveProject.tablePrefix ?? "wp_" },
                    set: { newValue in
                        Task {
                            try? await config.updateActiveProject { $0.tablePrefix = newValue }
                        }
                    }
                ))
                .textFieldStyle(.roundedBorder)
                Text("Database table prefix (e.g., wp_, c8c_)")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section("Multisite Configuration") {
                if config.safeActiveProject.hasSubTargets {
                    Text("\(config.safeActiveProject.subTargets.count) site(s) configured")
                        .font(.caption)
                        .foregroundColor(.secondary)

                    ForEach(config.safeActiveProject.subTargets) { subTarget in
                        HStack {
                            if subTarget.isDefault {
                                Image(systemName: "star.fill")
                                    .foregroundColor(.yellow)
                                    .font(.caption)
                            }
                            Text(subTarget.name)
                            Spacer()
                            Text(subTarget.domain)
                                .foregroundColor(.secondary)
                                .font(.caption)
                        }
                    }
                } else {
                    Text("No multisite configured")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }

                Text("Use the CLI to manage subtargets: homeboy project subtarget add")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}
