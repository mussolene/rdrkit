import Foundation

public struct RDRKitCommandError: LocalizedError, Sendable {
    public let command: String
    public let message: String
    public let exitCode: Int32

    public var errorDescription: String? {
        message.isEmpty ? "rdrkit failed with exit code \(exitCode)" : message
    }
}

public struct RDRKitCLI: Sendable {
    public let executableURL: URL

    public init(executableURL: URL) {
        self.executableURL = executableURL
    }

    public static func bundled() throws -> RDRKitCLI {
        let environment = ProcessInfo.processInfo.environment
        var candidates: [URL] = []
        if let override = environment["RDRKIT_EXECUTABLE"], !override.isEmpty {
            candidates.append(URL(fileURLWithPath: override))
        }
        if let bundled = Bundle.main.url(forAuxiliaryExecutable: "rdrkit") {
            candidates.append(bundled)
        }
        candidates.append(Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/rdrkit"))
        candidates.append(URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .appendingPathComponent("target/release/rdrkit"))
        candidates.append(URL(fileURLWithPath: "/opt/homebrew/bin/rdrkit"))
        candidates.append(URL(fileURLWithPath: "/usr/local/bin/rdrkit"))

        guard let executable = candidates.first(where: {
            FileManager.default.isExecutableFile(atPath: $0.path)
        }) else {
            throw RDRKitCommandError(
                command: "locate",
                message: "The rdrkit engine was not found inside the application.",
                exitCode: -1
            )
        }
        return RDRKitCLI(executableURL: executable)
    }

    public func list(image: URL) async throws -> ImageDescription {
        try await decode(["list", image.path, "--json"], as: ImageDescription.self)
    }

    public func mount(image: URL, object: UInt32) async throws -> MountResult {
        try await decode(
            ["mount", image.path, "--object", String(object), "--json"],
            as: MountResult.self
        )
    }

    public func status() async throws -> StatusResult {
        try await decode(["status", "--json"], as: StatusResult.self)
    }

    public func unmount(sessionID: String) async throws -> UnmountResult {
        try await decode(["unmount", sessionID, "--json"], as: UnmountResult.self)
    }

    private func decode<T: Decodable & Sendable>(
        _ arguments: [String],
        as type: T.Type
    ) async throws -> T {
        let data = try await run(arguments)
        let decoded = try JSONDecoder().decode(type, from: data)
        return decoded
    }

    private func run(_ arguments: [String]) async throws -> Data {
        let executableURL = executableURL
        return try await Task.detached(priority: .userInitiated) {
            let process = Process()
            let standardOutput = Pipe()
            let standardError = Pipe()
            process.executableURL = executableURL
            process.arguments = arguments
            process.standardInput = FileHandle.nullDevice
            process.standardOutput = standardOutput
            process.standardError = standardError

            var environment = ProcessInfo.processInfo.environment
            let systemPath = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
            if let inherited = environment["PATH"], !inherited.isEmpty {
                environment["PATH"] = "\(systemPath):\(inherited)"
            } else {
                environment["PATH"] = systemPath
            }
            process.environment = environment

            try process.run()
            let output = standardOutput.fileHandleForReading.readDataToEndOfFile()
            let error = standardError.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()

            guard process.terminationStatus == 0 else {
                throw RDRKitCommandError(
                    command: arguments.joined(separator: " "),
                    message: String(data: error, encoding: .utf8)?
                        .trimmingCharacters(in: .whitespacesAndNewlines) ?? "",
                    exitCode: process.terminationStatus
                )
            }
            return output
        }.value
    }
}
