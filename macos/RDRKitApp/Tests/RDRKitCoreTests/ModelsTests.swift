import Foundation
import Testing
@testable import RDRKitCore

@Test func decodesImageDescription() throws {
    let data = Data(#"{"schema_version":1,"image":"/tmp/disk.rdr","physical_size":1437,"objects":[{"id":0,"logical_size":1048576,"chunks":4,"chunk_size":262144}]}"#.utf8)
    let value = try JSONDecoder().decode(ImageDescription.self, from: data)

    #expect(value.schemaVersion == 1)
    #expect(value.objects.count == 1)
    #expect(value.objects[0].logicalSize == 1_048_576)
}

@Test func decodesMountedSession() throws {
    let data = Data(#"{"schema_version":1,"sessions":[{"session_id":"session-3","state":"active","image":"/tmp/disk.rdr","object":3,"device":"/dev/disk9","volumes":[{"source":"/dev/disk9","mount_point":"/Volumes/Data"}]}]}"#.utf8)
    let value = try JSONDecoder().decode(StatusResult.self, from: data)

    #expect(value.sessions[0].id == "session-3")
    #expect(value.sessions[0].volumes[0].mountPoint == "/Volumes/Data")
}
