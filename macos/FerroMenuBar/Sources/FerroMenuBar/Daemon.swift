import Foundation

// MARK: - JSON-RPC over the ferrod Unix socket (newline-delimited JSON).

enum DaemonError: LocalizedError {
    case connect(String)
    case rpc(String)

    var errorDescription: String? {
        switch self {
        case .connect(let message): return message
        case .rpc(let message): return message
        }
    }
}

struct Site: Codable, Identifiable {
    var id: String { name }
    let name: String
    let path: String
    let php_version: String
    let running: Bool
    let db_name: String?
    let db_user: String?
    let db_pass: String?
}

struct ServiceInfo: Codable, Identifiable {
    var id: String { name }
    let name: String
    let status: String
    let pid: Int?
    let restarts: Int?
    let detail: String?
}

struct ConflictInfo: Codable, Identifiable {
    var id: Int { port }
    let port: Int
    let pid: Int
    let command: String
    let fix: String
}

struct StatusReport: Codable {
    let router_running: Bool
    let http_port: Int
    let https_port: Int
    let services: [ServiceInfo]
    let conflicts: [ConflictInfo]
}

struct DaemonEvent: Codable {
    let ts: UInt64
    let kind: String
    let message: String
}

/// Minimal blocking JSON-RPC client. One connection per call — cheap at
/// this scale and keeps threading trivial.
enum DaemonClient {
    static let socketPath =
        NSHomeDirectory() + "/Library/Application Support/ferro/ferrod.sock"

    static func call(_ method: String, _ params: [String: Any] = [:]) throws -> Any {
        try call(method, paramsJSON: JSONSerialization.data(withJSONObject: params))
    }

    static func call(_ method: String, paramsJSON: Data) throws -> Any {
        let fd = try connect()
        defer { close(fd) }
        var data = Data()
        data.append(Data("{\"id\":1,\"method\":".utf8))
        data.append(try JSONSerialization.data(withJSONObject: method))
        data.append(Data(",\"params\":".utf8))
        data.append(paramsJSON)
        data.append(Data("}".utf8))
        data.append(0x0a)
        try writeAll(fd, data)
        guard let line = try readLine(fd) else {
            throw DaemonError.connect("ferrod closed the connection without a reply")
        }
        let parsed = try JSONSerialization.jsonObject(with: line) as? [String: Any] ?? [:]
        if let error = parsed["error"] as? [String: Any],
           let message = error["message"] as? String {
            throw DaemonError.rpc(message)
        }
        return parsed["result"] ?? NSNull()
    }

    static func callDecodable<T: Decodable>(
        _ method: String, _ params: [String: Any] = [:], as type: T.Type
    ) throws -> T {
        let result = try call(method, params)
        let data = try JSONSerialization.data(withJSONObject: result)
        return try JSONDecoder().decode(T.self, from: data)
    }

    // MARK: socket plumbing

    static func connect() throws -> Int32 {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw DaemonError.connect("socket() failed") }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let path = socketPath.utf8CString
        guard path.count <= 104 else {
            close(fd)
            throw DaemonError.connect("socket path too long")
        }
        withUnsafeMutablePointer(to: &addr.sun_path) { sunPath in
            sunPath.withMemoryRebound(to: CChar.self, capacity: 104) { dest in
                path.withUnsafeBytes { src in
                    _ = memcpy(dest, src.baseAddress!, path.count)
                }
            }
        }
        let result = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard result == 0 else {
            close(fd)
            throw DaemonError.connect(
                "cannot connect to ferrod — is it running? (ferro daemon start)")
        }
        return fd
    }

    static func writeAll(_ fd: Int32, _ data: Data) throws {
        try data.withUnsafeBytes { buffer in
            var written = 0
            while written < data.count {
                let n = Darwin.write(fd, buffer.baseAddress! + written, data.count - written)
                guard n > 0 else { throw DaemonError.connect("write to ferrod failed") }
                written += n
            }
        }
    }

    /// Read one \n-terminated line. Buffered, so responses of a few KB
    /// cost a handful of syscalls.
    static func readLine(_ fd: Int32) throws -> Data? {
        var out = Data()
        var chunk = [UInt8](repeating: 0, count: 8192)
        while true {
            let n = Darwin.read(fd, &chunk, chunk.count)
            guard n > 0 else { return out.isEmpty ? nil : out }
            for byte in chunk[..<n] {
                if byte == 0x0a { return out }
                out.append(byte)
            }
        }
    }
}

/// Long-lived event subscription (`events` RPC): one JSON event per
/// line until the connection drops, then reconnect with backoff.
final class EventStream: @unchecked Sendable {
    private var task: Task<Void, Never>?

    func start(onEvent: @escaping @Sendable (DaemonEvent) -> Void) {
        task?.cancel()
        task = Task.detached { [weak self] in
            var delay: UInt64 = 1
            while !Task.isCancelled {
                guard let self else { return }
                do {
                    try await self.run(onEvent: onEvent)
                    delay = 1
                } catch {
                    try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
                    delay = min(delay * 2, 15)
                }
            }
        }
    }

    private func run(onEvent: @escaping @Sendable (DaemonEvent) -> Void) throws {
        let fd = try DaemonClient.connect()
        defer { close(fd) }
        var request = try JSONSerialization.data(withJSONObject: [
            "id": 1, "method": "events", "params": [String: Any](),
        ])
        request.append(0x0a)
        try DaemonClient.writeAll(fd, request)
        // First line is the subscription acknowledgement.
        _ = try DaemonClient.readLine(fd)
        while !Task.isCancelled {
            guard let line = try DaemonClient.readLine(fd) else { return }
            if let event = try? JSONDecoder().decode(DaemonEvent.self, from: line) {
                onEvent(event)
            }
        }
    }

    deinit { task?.cancel() }
}
