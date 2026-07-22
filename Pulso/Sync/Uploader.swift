import Foundation

struct UploadResult: Sendable {
    let status: Int
    let bodySnippet: String
    let duration: TimeInterval

    var isSuccess: Bool { (200 ..< 300).contains(status) }
}

protocol Uploading: Sendable {
    func send(_ body: Data, gzipped: Bool, to url: URL, token: String?) async throws -> UploadResult
}

struct HTTPUploader: Uploading {
    private let session: URLSession

    init() {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        configuration.timeoutIntervalForResource = 120
        configuration.waitsForConnectivity = false // fail fast; the outbox owns retries
        session = URLSession(configuration: configuration)
    }

    func send(_ body: Data, gzipped: Bool, to url: URL, token: String?) async throws -> UploadResult {
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if gzipped {
            request.setValue("gzip", forHTTPHeaderField: "Content-Encoding")
        }
        if let token, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        let started = Date()
        let (data, response) = try await session.upload(for: request, from: body)
        return UploadResult(
            status: (response as? HTTPURLResponse)?.statusCode ?? 0,
            bodySnippet: String(decoding: data.prefix(300), as: UTF8.self),
            duration: Date().timeIntervalSince(started)
        )
    }

    /// GET <base>/latest — per-type newest sample timestamp on the server.
    /// nil on any failure (older servers without the endpoint included);
    /// callers fall back to a full backfill.
    func fetchLatest(base: URL, token: String?) async -> [String: String]? {
        guard let url = URL(string: base.absoluteString + "/latest") else { return nil }
        var request = URLRequest(url: url)
        request.timeoutInterval = 10
        if let token, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        guard let (data, response) = try? await session.data(for: request),
              (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
        return try? JSONDecoder().decode([String: String].self, from: data)
    }

    /// GET <base>/health — the Settings "Test Connection" button.
    func checkHealth(_ url: URL) async -> String {
        var request = URLRequest(url: url)
        request.timeoutInterval = 10
        do {
            let (data, response) = try await session.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            if (200 ..< 300).contains(status) {
                return "OK — HTTP \(status)"
            }
            return "HTTP \(status): \(String(decoding: data.prefix(120), as: UTF8.self))"
        } catch {
            return "Unreachable: \(error.localizedDescription)"
        }
    }
}
