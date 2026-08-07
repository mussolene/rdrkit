import Foundation

public struct RDRObject: Codable, Identifiable, Hashable, Sendable {
    public let id: UInt32
    public let logicalSize: UInt64
    public let chunks: Int
    public let chunkSize: UInt32

    enum CodingKeys: String, CodingKey {
        case id
        case logicalSize = "logical_size"
        case chunks
        case chunkSize = "chunk_size"
    }
}

public struct ImageDescription: Codable, Sendable {
    public let schemaVersion: Int
    public let image: String
    public let physicalSize: UInt64
    public let objects: [RDRObject]

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case image
        case physicalSize = "physical_size"
        case objects
    }
}

public struct MountedVolume: Codable, Hashable, Sendable {
    public let source: String
    public let mountPoint: String

    enum CodingKeys: String, CodingKey {
        case source
        case mountPoint = "mount_point"
    }
}

public struct MountResult: Codable, Sendable {
    public let schemaVersion: Int
    public let sessionID: String
    public let state: String
    public let image: String
    public let object: UInt32
    public let logicalSize: UInt64
    public let device: String?
    public let volumes: [MountedVolume]

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case sessionID = "session_id"
        case state
        case image
        case object
        case logicalSize = "logical_size"
        case device
        case volumes
    }
}

public struct MountSession: Codable, Identifiable, Sendable {
    public var id: String { sessionID }

    public let sessionID: String
    public let state: String
    public let image: String
    public let object: UInt32
    public let device: String?
    public let volumes: [MountedVolume]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case state
        case image
        case object
        case device
        case volumes
    }

    public func matches(imageURL: URL, objectID: UInt32) -> Bool {
        object == objectID
            && URL(fileURLWithPath: image).standardizedFileURL == imageURL.standardizedFileURL
    }
}

public struct StatusResult: Codable, Sendable {
    public let schemaVersion: Int
    public let sessions: [MountSession]

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case sessions
    }
}

public struct UnmountResult: Codable, Sendable {
    public let schemaVersion: Int
    public let sessionID: String
    public let state: String
    public let image: String

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case sessionID = "session_id"
        case state
        case image
    }
}

public enum ByteCount {
    public static func format(_ value: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .file)
    }
}
