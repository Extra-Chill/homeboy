import ArgumentParser
import Foundation

struct Docs: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "docs",
        abstract: "Display comprehensive CLI documentation",
        discussion: """
            Shows CLI documentation from the bundled CLI.md file.

            Usage:
              homeboy docs              # Full documentation
              homeboy docs <topic>      # Filter by topic

            Examples:
              homeboy docs deploy       # Deploy command docs
              homeboy docs project set  # Project set subcommand docs

            Topics: projects, project, server, wp, pm2, db, deploy, ssh, module
            """
    )

    @Argument(parsing: .captureForPassthrough, help: "Topic to filter (e.g., 'deploy', 'project set')")
    var topic: [String] = []

    func run() throws {
        let content = try loadDocumentation()

        if topic.isEmpty {
            displayWithPager(content)
        } else {
            let searchTopic = topic.joined(separator: " ")
            let filtered = filterToTopic(content, topic: searchTopic)

            if filtered.isEmpty {
                fputs("No documentation found for '\(searchTopic)'.\n", stderr)
                fputs("Available topics: projects, project, server, wp, pm2, db, deploy, ssh, module\n", stderr)
                throw ExitCode.failure
            }
            print(filtered)
        }
    }

    private func loadDocumentation() throws -> String {
        let paths = [
            AppPaths.homeboy.appendingPathComponent("docs/CLI.md").path,
            Bundle.main.resourceURL?.appendingPathComponent("docs/CLI.md").path
        ].compactMap { $0 }

        for path in paths {
            if let content = try? String(contentsOfFile: path, encoding: .utf8) {
                return content
            }
        }

        fputs("CLI.md not found.\n", stderr)
        fputs("Documentation available at: https://github.com/Extra-Chill/homeboy/blob/main/docs/CLI.md\n", stderr)
        throw ExitCode.failure
    }

    private func filterToTopic(_ content: String, topic: String) -> String {
        let lines = content.components(separatedBy: "\n")
        var result: [String] = []
        var capturing = false
        var captureDepth = 0
        let normalizedTopic = topic.lowercased().trimmingCharacters(in: .whitespaces)

        for line in lines {
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if trimmed.hasPrefix("#") {
                let depth = trimmed.prefix(while: { $0 == "#" }).count
                let heading = String(trimmed.dropFirst(depth))
                    .trimmingCharacters(in: .whitespaces)
                    .lowercased()

                if heading == normalizedTopic ||
                   heading.hasPrefix(normalizedTopic + " ") ||
                   heading.contains(normalizedTopic) {
                    capturing = true
                    captureDepth = depth
                    result.append(line)
                } else if capturing && depth <= captureDepth {
                    break
                } else if capturing {
                    result.append(line)
                }
            } else if capturing {
                result.append(line)
            }
        }

        return result.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func displayWithPager(_ content: String) {
        guard isatty(STDOUT_FILENO) != 0 else {
            print(content)
            return
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/less")
        process.arguments = ["-R"]

        let pipe = Pipe()
        process.standardInput = pipe

        do {
            try process.run()
            pipe.fileHandleForWriting.write(content.data(using: .utf8) ?? Data())
            pipe.fileHandleForWriting.closeFile()
            process.waitUntilExit()
        } catch {
            print(content)
        }
    }
}
