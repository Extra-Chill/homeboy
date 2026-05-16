import Foundation

actor HomeboyDaemonClient {
    private let baseURL: URL
    private let decoder: JSONDecoder

    init(baseURL: String) throws {
        guard let url = URL(string: baseURL) else {
            throw URLError(.badURL)
        }
        self.baseURL = url
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        self.decoder = decoder
    }

    func listJobs() async throws -> [DaemonJob] {
        let response: DaemonAPIResponse<DaemonJobsOutput> = try await request("/jobs")
        return response.body.jobs
    }

    func showJob(id: String) async throws -> DaemonJob {
        let response: DaemonAPIResponse<DaemonJobOutput> = try await request("/jobs/\(id)")
        return response.body.job
    }

    func jobEvents(id: String) async throws -> [DaemonJobEvent] {
        let response: DaemonAPIResponse<DaemonJobEventsOutput> = try await request("/jobs/\(id)/events")
        return response.body.events
    }

    func cancelJob(id: String) async throws -> DaemonJob {
        let response: DaemonAPIResponse<DaemonJobOutput> = try await request("/jobs/\(id)/cancel", method: "POST")
        return response.body.job
    }

    func enqueue(kind: String, body: [String: JSONValue]) async throws -> DaemonJobEnqueueOutput {
        let response: DaemonAPIResponse<DaemonJobEnqueueOutput> = try await request(
            "/\(kind)",
            method: "POST",
            body: JSONValue.object(body)
        )
        return response.body
    }

    func listArtifacts(runId: String) async throws -> [RunArtifact] {
        let response: DaemonAPIResponse<RunsArtifactsOutput> = try await request("/runs/\(runId)/artifacts")
        return response.body.artifacts
    }

    func artifactSyncManifest(runId: String) async throws -> [RunArtifactSyncItem] {
        let response: DaemonAPIResponse<RunsArtifactSyncOutput> = try await request("/runs/\(runId)/artifacts/sync")
        return response.body.artifacts
    }

    func downloadArtifact(runId: String, artifactId: String) async throws -> URL {
        let url = baseURL.appending(path: "runs/\(runId)/artifacts/\(artifactId)")
        let (downloadURL, response) = try await URLSession.shared.download(from: url)
        guard let httpResponse = response as? HTTPURLResponse, (200..<300).contains(httpResponse.statusCode) else {
            throw URLError(.badServerResponse)
        }

        let suggestedName = response.suggestedFilename ?? artifactId
        let destination = FileManager.default.temporaryDirectory
            .appendingPathComponent("homeboy-artifact-\(UUID().uuidString)-\(suggestedName)")
        try FileManager.default.moveItem(at: downloadURL, to: destination)
        return destination
    }

    private func request<T: Decodable>(
        _ path: String,
        method: String = "GET",
        body: JSONValue? = nil
    ) async throws -> T {
        let url = baseURL.appending(path: path.trimmingCharacters(in: CharacterSet(charactersIn: "/")))
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        if let body {
            request.httpBody = try JSONEncoder().encode(body)
        }

        let (data, response) = try await URLSession.shared.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse else {
            throw URLError(.badServerResponse)
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            throw HomeboyDaemonError(statusCode: httpResponse.statusCode, body: String(data: data, encoding: .utf8) ?? "")
        }

        let envelope = try decoder.decode(DaemonEnvelope<T>.self, from: data)
        guard envelope.success, let decoded = envelope.data else {
            throw HomeboyDaemonError(statusCode: httpResponse.statusCode, body: String(data: data, encoding: .utf8) ?? "")
        }
        return decoded
    }
}

struct HomeboyDaemonError: LocalizedError {
    let statusCode: Int
    let body: String

    var errorDescription: String? {
        "Homeboy daemon request failed (HTTP \(statusCode)): \(body)"
    }
}
