import ArgumentParser
import Foundation

@main
struct HomeboyCLI: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "homeboy",
        abstract: "CLI for WordPress development and deployment",
        version: "0.3.0",
        subcommands: [WP.self, DB.self, Deploy.self, Projects.self],
        defaultSubcommand: nil
    )
}

/// Lists available projects or shows current project
struct Projects: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "projects",
        abstract: "List available projects"
    )
    
    @Flag(name: .long, help: "Show only the current active project")
    var current: Bool = false
    
    func run() throws {
        let currentProject = ConfigurationManager.readCurrentProject()
        let availableIds = getAvailableProjectIds()
        
        if current {
            print(currentProject.id)
        } else {
            if availableIds.isEmpty {
                print("No projects configured. Open Homeboy.app to set up a project.")
            } else {
                for id in availableIds {
                    let marker = id == currentProject.id ? " (active)" : ""
                    print("\(id)\(marker)")
                }
            }
        }
    }
    
    private func getAvailableProjectIds() -> [String] {
        let fileManager = FileManager.default
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let projectsDir = appSupport.appendingPathComponent("Homeboy/projects")
        
        guard let files = try? fileManager.contentsOfDirectory(at: projectsDir, includingPropertiesForKeys: nil) else {
            return []
        }
        
        return files
            .filter { $0.pathExtension == "json" }
            .map { $0.deletingPathExtension().lastPathComponent }
            .sorted()
    }
}
