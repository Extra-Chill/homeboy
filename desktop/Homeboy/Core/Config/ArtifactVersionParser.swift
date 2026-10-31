import Foundation

struct ArtifactVersionParser {

    static func parseVersion(
        fromArtifact artifactPath: String,
        componentId: String,
        versionFile: String,
        versionPattern: String? = nil
    ) -> String? {
        let ext = (artifactPath as NSString).pathExtension.lowercased()
        let internalPath = "\(componentId)/\(versionFile)"

        guard let content = extractFileContent(
            from: artifactPath,
            internalPath: internalPath,
            archiveType: ext
        ) else {
            return nil
        }

        return VersionParser.parseVersion(from: content, pattern: versionPattern)
    }

    private static func extractFileContent(
        from archivePath: String,
        internalPath: String,
        archiveType: String
    ) -> String? {
        let process = Process()
        let pipe = Pipe()

        switch archiveType {
        case "zip":
            process.executableURL = URL(fileURLWithPath: "/usr/bin/unzip")
            process.arguments = ["-p", archivePath, internalPath]

        case "gz", "tgz":
            process.executableURL = URL(fileURLWithPath: "/usr/bin/tar")
            process.arguments = ["-xOf", archivePath, internalPath]

        default:
            return nil
        }

        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
            process.waitUntilExit()

            guard process.terminationStatus == 0 else { return nil }

            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            return String(data: data, encoding: .utf8)
        } catch {
            return nil
        }
    }

    static func artifactExists(at path: String) -> Bool {
        FileManager.default.fileExists(atPath: path)
    }

    static func artifactModificationDate(at path: String) -> Date? {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: path),
              let modDate = attributes[.modificationDate] as? Date else {
            return nil
        }
        return modDate
    }
}
