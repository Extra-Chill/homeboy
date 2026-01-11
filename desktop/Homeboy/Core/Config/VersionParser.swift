import Foundation

struct VersionParser {

    static func parseVersion(from content: String, pattern: String? = nil) -> String? {
        if let pattern = pattern {
            guard let regex = try? NSRegularExpression(pattern: pattern) else {
                return nil
            }
            let range = NSRange(content.startIndex..., in: content)
            guard let match = regex.firstMatch(in: content, options: [], range: range),
                  let versionRange = Range(match.range(at: 1), in: content) else {
                return nil
            }
            return String(content[versionRange])
        }

        let defaultPattern = #"Version:\s*([0-9]+\.[0-9]+\.?[0-9]*)"#
        guard let regex = try? NSRegularExpression(pattern: defaultPattern) else {
            return nil
        }
        let range = NSRange(content.startIndex..., in: content)
        guard let match = regex.firstMatch(in: content, options: [], range: range),
              let versionRange = Range(match.range(at: 1), in: content) else {
            return nil
        }
        return String(content[versionRange])
    }
}
