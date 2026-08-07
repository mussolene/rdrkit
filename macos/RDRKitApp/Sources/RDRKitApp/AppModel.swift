import AppKit
import Foundation
import RDRKitCore
import UniformTypeIdentifiers

extension UTType {
    static let rdrImage = UTType(importedAs: "com.mussolene.rdrkit.rdr-image")
}

@MainActor
final class AppModel: ObservableObject {
    static let shared = AppModel()

    @Published private(set) var imageURL: URL?
    @Published private(set) var imageDescription: ImageDescription?
    @Published var selectedObjectID: UInt32?
    @Published private(set) var sessions: [MountSession] = []
    @Published private(set) var isBusy = false
    @Published private(set) var statusMessage: String?
    @Published var errorMessage: String?

    private init() {}

    func chooseImage() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.rdrImage]
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.message = "Choose an RDR disk image"
        panel.prompt = "Open"

        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { await openImage(url) }
    }

    func openImage(_ url: URL) async {
        guard url.pathExtension.lowercased() == "rdr" else {
            errorMessage = "Choose a file with the .rdr extension."
            return
        }

        await perform("Reading image") { cli in
            let description = try await cli.list(image: url)
            imageURL = url
            imageDescription = description
            selectedObjectID = description.objects.max(by: { $0.logicalSize < $1.logicalSize })?.id

            if description.objects.count == 1, let object = description.objects.first {
                statusMessage = "Mounting the only disk object"
                _ = try await cli.mount(image: url, object: object.id)
                try await loadSessions(using: cli)
            } else if description.objects.isEmpty {
                statusMessage = "The image contains no disk objects"
            } else {
                statusMessage = "Select one disk object or mount all objects"
            }
        }
    }

    func mountSelected() async {
        guard let imageURL, let selectedObjectID else { return }
        await perform("Mounting disk object (selectedObjectID)") { cli in
            _ = try await cli.mount(image: imageURL, object: selectedObjectID)
            try await loadSessions(using: cli)
        }
    }

    func mountAll() async {
        guard let imageURL, let objects = imageDescription?.objects, !objects.isEmpty else { return }
        await perform("Mounting all disk objects") { cli in
            for object in objects {
                statusMessage = "Mounting disk object (object.id)"
                _ = try await cli.mount(image: imageURL, object: object.id)
            }
            try await loadSessions(using: cli)
        }
    }

    func refreshStatus() async {
        await perform("Refreshing mounted disks") { cli in
            try await loadSessions(using: cli)
        }
    }

    func unmount(_ session: MountSession) async {
        await perform("Unmounting disk object (session.object)") { cli in
            _ = try await cli.unmount(sessionID: session.sessionID)
            try await loadSessions(using: cli)
        }
    }

    func openInFinder(_ volume: MountedVolume) {
        NSWorkspace.shared.open(URL(fileURLWithPath: volume.mountPoint, isDirectory: true))
    }

    private func perform(
        _ activity: String,
        operation: (RDRKitCLI) async throws -> Void
    ) async {
        guard !isBusy else { return }
        isBusy = true
        errorMessage = nil
        statusMessage = activity
        defer { isBusy = false }

        do {
            let cli = try RDRKitCLI.bundled()
            try await operation(cli)
            if errorMessage == nil {
                statusMessage = nil
            }
        } catch {
            errorMessage = error.localizedDescription
            statusMessage = nil
        }
    }

    private func loadSessions(using cli: RDRKitCLI) async throws {
        sessions = try await cli.status().sessions
    }
}
