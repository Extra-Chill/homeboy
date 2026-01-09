import ArgumentParser
import Foundation

struct Module: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "module",
        abstract: "Execute CLI-compatible Homeboy modules",
        discussion: """
            Execute CLI-compatible modules from the command line.

            Subcommands:
              list   Show available modules
              run    Execute a module

            Examples:
              homeboy module list
              homeboy module list --project extrachill
              homeboy module run my-module --target_url https://example.com

            Note: Only modules with 'cli' runtime type can be run from CLI.

            See 'homeboy docs module' for full documentation.
            """,
        subcommands: [ModuleRun.self, ModuleList.self],
        defaultSubcommand: ModuleList.self
    )
}

// MARK: - Module Run

struct ModuleRun: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "run",
        abstract: "Execute a CLI-compatible module",
        discussion: """
            Executes a CLI-compatible module.

            Examples:
              homeboy module run my-module --target_url https://example.com
              homeboy module run my-module --project extrachill arg1 arg2

            Prerequisites:
              - Module must have 'cli' runtime type
              - Project must have localCLI.sitePath configured
            """
    )
    
    @Argument(help: "Module ID")
    var moduleId: String
    
    @Option(name: .shortAndLong, help: "Project ID (defaults to active project)")
    var project: String?
    
    @Argument(parsing: .captureForPassthrough, help: "Arguments to pass to the module")
    var args: [String] = []
    
    func run() throws {
        // Determine project ID
        let projectId = project ?? getActiveProjectId()
        
        guard let projectId = projectId else {
            fputs("Error: No project specified and no active project set\n", stderr)
            throw ExitCode.failure
        }
        
        // Load project configuration
        guard let projectConfig = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        // Load project type definition
        guard let typeDefinition = loadProjectTypeDefinition(id: projectConfig.projectType) else {
            fputs("Error: Unknown project type '\(projectConfig.projectType)'\n", stderr)
            throw ExitCode.failure
        }
        
        // Load module manifest
        guard let module = loadModule(id: moduleId) else {
            fputs("Error: Module '\(moduleId)' not found\n", stderr)
            fputs("Use 'homeboy module list' to see available modules.\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate module requirements
        var validationFailed = false
        var errorMessages: [String] = []

        if let requires = module.requires {
            // Check project type requirement
            if let requiredType = requires.projectType, requiredType != projectConfig.projectType {
                errorMessages.append("Module '\(moduleId)' requires project type '\(requiredType)' but project '\(projectId)' is '\(projectConfig.projectType)'")
                validationFailed = true
            }

            // Check component requirements
            if let requiredComponents = requires.components, !requiredComponents.isEmpty {
                let installedComponentIds = Set(projectConfig.components.map { $0.id })
                let missing = requiredComponents.filter { !installedComponentIds.contains($0) }
                if !missing.isEmpty {
                    errorMessages.append("Module '\(moduleId)' requires components not configured in project '\(projectId)': \(missing.joined(separator: ", "))")
                    validationFailed = true
                }
            }

            // Check feature requirements
            if let requiredFeatures = requires.features {
                for feature in requiredFeatures {
                    let isSatisfied: Bool
                    switch feature {
                    case "hasCLI":
                        isSatisfied = typeDefinition.hasCLI
                    case "hasDatabase", "hasDeployer", "hasRemoteDeployment", "hasRemoteLogs", "hasRemoteFileEditor":
                        isSatisfied = true  // Universal features
                    default:
                        isSatisfied = false
                    }
                    if !isSatisfied {
                        errorMessages.append("Module '\(moduleId)' requires feature '\(feature)' which is not available for project type '\(typeDefinition.displayName)'")
                        validationFailed = true
                    }
                }
            }
        }

        if validationFailed {
            for msg in errorMessages {
                fputs("Error: \(msg)\n", stderr)
            }

            // Find and suggest compatible projects
            let compatible = findCompatibleProjects(for: module).filter { $0 != projectId }
            if compatible.count == 1 {
                fputs("\nCompatible project found: \(compatible[0])\n", stderr)
                fputs("  Try: homeboy module run \(moduleId) --project \(compatible[0])\n", stderr)
            } else if compatible.count > 1 {
                fputs("\nCompatible projects: \(compatible.joined(separator: ", "))\n", stderr)
                fputs("  Use --project to specify which one.\n", stderr)
            }

            throw ExitCode.failure
        }

        // Validate runtime type
        guard module.runtime.type == .cli else {
            fputs("Error: Module '\(moduleId)' has runtime type '\(module.runtime.type.rawValue)' which is not supported by CLI\n", stderr)
            fputs("Only modules with 'cli' runtime type can be run from the command line.\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate CLI is configured for project type
        guard let cliConfig = typeDefinition.cli else {
            fputs("Error: Project type '\(typeDefinition.displayName)' does not support CLI\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate local CLI is configured
        guard projectConfig.localCLI.isConfigured else {
            fputs("Error: Local CLI not configured for project '\(projectId)'\n", stderr)
            fputs("Configure 'Local Site Path' in Homeboy.app Settings.\n", stderr)
            throw ExitCode.failure
        }
        
        // Build module args from manifest and CLI args
        let moduleArgs = buildModuleArgs(module: module, cliArgs: args)
        
        // Build template variables
        let localDomain = projectConfig.localCLI.domain.isEmpty ? "localhost" : projectConfig.localCLI.domain
        let cliPath = projectConfig.localCLI.cliPath ?? cliConfig.defaultCLIPath ?? cliConfig.tool
        
        let variables: [String: String] = [
            TemplateRenderer.Variables.projectId: projectConfig.id,
            TemplateRenderer.Variables.domain: localDomain,
            TemplateRenderer.Variables.sitePath: projectConfig.localCLI.sitePath,
            TemplateRenderer.Variables.cliPath: cliPath,
            TemplateRenderer.Variables.args: moduleArgs
        ]
        
        // Render command from template
        let command = TemplateRenderer.render(cliConfig.commandTemplate, variables: variables)
        
        // Show command being run
        fputs("$ \(command)\n\n", stderr)
        
        // Execute locally
        let result = executeLocalCommand(command)
        
        // Output result
        print(result.output, terminator: "")
        
        if !result.success {
            throw ExitCode.failure
        }
    }
    
    /// Builds the args string for the module from manifest config and CLI args
    private func buildModuleArgs(module: ModuleManifest, cliArgs: [String]) -> String {
        var parts: [String] = []

        // Add args template if present
        if let argsTemplate = module.runtime.args {
            parts.append(argsTemplate)
        }

        // Add CLI args as additional arguments
        if !cliArgs.isEmpty {
            parts.append(contentsOf: cliArgs)
        }

        return parts.joined(separator: " ")
    }
}

// MARK: - Module List

struct ModuleList: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "list",
        abstract: "Show installed modules with compatibility status",
        discussion: """
            Shows installed modules with compatibility status.

            Examples:
              homeboy module list                        # All modules
              homeboy module list --project extrachill   # Show compatibility
            """
    )
    
    @Option(name: .shortAndLong, help: "Project ID to filter compatible modules")
    var project: String?
    
    func run() throws {
        let modules = loadAllModules()
        
        if modules.isEmpty {
            print("No modules installed.")
            print("Modules are installed at: ~/Library/Application Support/Homeboy/modules/")
            return
        }
        
        // Optionally filter by project compatibility
        let projectConfig: ProjectConfiguration?
        if let projectId = project {
            projectConfig = loadProjectConfig(id: projectId)
        } else {
            projectConfig = nil
        }
        
        print("Available modules:\n")
        
        for module in modules.sorted(by: { $0.id < $1.id }) {
            let compatible = isModuleCompatible(module, with: projectConfig)
            let marker = compatible ? "✓" : "✗"
            let runtime = module.runtime.type.rawValue
            
            print("  \(marker) \(module.id)")
            print("    \(module.name) (v\(module.version))")
            print("    Runtime: \(runtime)")
            if let desc = module.description.split(separator: "\n").first {
                print("    \(desc)")
            }
            print()
        }
        
        if projectConfig != nil {
            print("✓ = compatible with project, ✗ = not compatible")
        }
    }
    
    private func isModuleCompatible(_ module: ModuleManifest, with project: ProjectConfiguration?) -> Bool {
        guard let project = project, let requires = module.requires else {
            return true
        }

        // Check project type
        if let requiredType = requires.projectType, requiredType != project.projectType {
            return false
        }

        // Check components
        if let requiredComponents = requires.components, !requiredComponents.isEmpty {
            let installedComponentIds = Set(project.components.map { $0.id })
            for component in requiredComponents {
                if !installedComponentIds.contains(component) {
                    return false
                }
            }
        }

        // Check features
        if let requiredFeatures = requires.features {
            let typeDefinition = loadProjectTypeDefinition(id: project.projectType) ?? .fallbackGeneric
            for feature in requiredFeatures {
                let isSatisfied: Bool
                switch feature {
                case "hasCLI":
                    isSatisfied = typeDefinition.hasCLI
                case "hasDatabase", "hasDeployer", "hasRemoteDeployment", "hasRemoteLogs", "hasRemoteFileEditor":
                    isSatisfied = true
                default:
                    isSatisfied = false
                }
                if !isSatisfied {
                    return false
                }
            }
        }

        return true
    }
}

// MARK: - Module Loading

/// Loads a module manifest by ID
func loadModule(id: String) -> ModuleManifest? {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let modulePath = appSupport.appendingPathComponent("Homeboy/modules/\(id)/module.json")
    
    guard let data = try? Data(contentsOf: modulePath),
          var module = try? JSONDecoder().decode(ModuleManifest.self, from: data) else {
        return nil
    }
    
    module.modulePath = modulePath.deletingLastPathComponent().path
    return module
}

/// Loads all installed modules
func loadAllModules() -> [ModuleManifest] {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let modulesDir = appSupport.appendingPathComponent("Homeboy/modules")
    
    guard let contents = try? fileManager.contentsOfDirectory(at: modulesDir, includingPropertiesForKeys: nil) else {
        return []
    }
    
    return contents.compactMap { dir -> ModuleManifest? in
        let manifestPath = dir.appendingPathComponent("module.json")
        guard let data = try? Data(contentsOf: manifestPath),
              var module = try? JSONDecoder().decode(ModuleManifest.self, from: data) else {
            return nil
        }
        module.modulePath = dir.path
        return module
    }
}

/// Gets the active project ID from app config
func getActiveProjectId() -> String? {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let configPath = appSupport.appendingPathComponent("Homeboy/config.json")

    guard let data = try? Data(contentsOf: configPath),
          let config = try? JSONDecoder().decode(AppConfiguration.self, from: data) else {
        return nil
    }

    return config.activeProjectId
}

/// Loads all project configurations
func loadAllProjectConfigs() -> [ProjectConfiguration] {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let projectsDir = appSupport.appendingPathComponent("Homeboy/projects")

    guard let files = try? fileManager.contentsOfDirectory(at: projectsDir, includingPropertiesForKeys: nil) else {
        return []
    }

    return files.compactMap { file -> ProjectConfiguration? in
        guard file.pathExtension == "json",
              let data = try? Data(contentsOf: file),
              let config = try? JSONDecoder().decode(ProjectConfiguration.self, from: data) else {
            return nil
        }
        return config
    }
}

/// Finds all projects compatible with a module's requirements
func findCompatibleProjects(for module: ModuleManifest) -> [String] {
    let allProjects = loadAllProjectConfigs()
    return allProjects.filter { project in
        guard let requires = module.requires else { return true }

        // Check project type
        if let requiredType = requires.projectType, requiredType != project.projectType {
            return false
        }

        // Check components
        if let requiredComponents = requires.components, !requiredComponents.isEmpty {
            let installedComponentIds = Set(project.components.map { $0.id })
            for component in requiredComponents {
                if !installedComponentIds.contains(component) {
                    return false
                }
            }
        }

        // Check features
        if let requiredFeatures = requires.features {
            let typeDefinition = loadProjectTypeDefinition(id: project.projectType) ?? .fallbackGeneric
            for feature in requiredFeatures {
                let isSatisfied: Bool
                switch feature {
                case "hasCLI":
                    isSatisfied = typeDefinition.hasCLI
                case "hasDatabase", "hasDeployer", "hasRemoteDeployment", "hasRemoteLogs", "hasRemoteFileEditor":
                    isSatisfied = true
                default:
                    isSatisfied = false
                }
                if !isSatisfied {
                    return false
                }
            }
        }

        // Check local CLI is configured (required for module run)
        if !project.localCLI.isConfigured {
            return false
        }

        return true
    }.map { $0.id }
}
