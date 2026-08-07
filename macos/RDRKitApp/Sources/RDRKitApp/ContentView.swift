import RDRKitCore
import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    imageSection
                    sessionsSection
                }
                .padding(24)
            }
            if model.isBusy || model.statusMessage != nil {
                Divider()
                statusBar
            }
        }
        .frame(minWidth: 720, minHeight: 520)
        .task { await model.refreshStatus() }
        .onOpenURL { url in
            Task { await model.openImage(url) }
        }
        .alert(
            "RDRKit could not complete the operation",
            isPresented: Binding(
                get: { model.errorMessage != nil },
                set: { if !$0 { model.errorMessage = nil } }
            ),
            actions: { Button("OK", role: .cancel) {} },
            message: { Text(model.errorMessage ?? "Unknown error") }
        )
        .onDrop(of: [UTType.fileURL.identifier], isTargeted: nil) { providers in
            guard let provider = providers.first else { return false }
            provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { item, _ in
                let url: URL?
                if let data = item as? Data {
                    url = URL(dataRepresentation: data, relativeTo: nil)
                } else {
                    url = item as? URL
                }
                if let url {
                    Task { @MainActor in await model.openImage(url) }
                }
            }
            return true
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            Image(systemName: "externaldrive.fill")
                .font(.title2)
                .foregroundStyle(.tint)
            Text("RDRKit")
                .font(.title2.weight(.semibold))
            Text("READ ONLY")
                .font(.caption2.weight(.bold))
                .padding(.horizontal, 7)
                .padding(.vertical, 3)
                .background(.green.opacity(0.16), in: Capsule())
                .foregroundStyle(.green)
            Spacer()
            Button("Refresh", systemImage: "arrow.clockwise") {
                Task { await model.refreshStatus() }
            }
            .disabled(model.isBusy)
            Button("Open Image", systemImage: "folder") {
                model.chooseImage()
            }
            .keyboardShortcut("o", modifiers: .command)
            .disabled(model.isBusy)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    @ViewBuilder
    private var imageSection: some View {
        if let image = model.imageDescription, let imageURL = model.imageURL {
            VStack(alignment: .leading, spacing: 12) {
                sectionTitle("Disk image", detail: imageURL.lastPathComponent)
                HStack(alignment: .top, spacing: 8) {
                    Text("Source")
                        .foregroundStyle(.secondary)
                    Text(imageURL.path)
                        .textSelection(.enabled)
                        .lineLimit(2)
                        .truncationMode(.middle)
                    Spacer()
                    Text("\(model.currentImageSessions.count) of \(image.objects.count) mounted")
                        .foregroundStyle(
                            model.currentImageSessions.isEmpty
                                ? Color(nsColor: .secondaryLabelColor)
                                : Color.green
                        )
                }
                .font(.caption)
                VStack(spacing: 0) {
                    ForEach(image.objects) { object in
                        objectRow(object)
                        if object.id != image.objects.last?.id { Divider() }
                    }
                }
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 10))

                HStack {
                    Button("Mount Selected", systemImage: "externaldrive.badge.plus") {
                        Task { await model.mountSelected() }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(
                        model.selectedObjectID == nil
                            || model.selectedObjectSession != nil
                            || model.isBusy
                    )

                    Button(model.currentImageSessions.isEmpty ? "Mount All Objects" : "Mount Remaining Objects") {
                        Task { await model.mountAll() }
                    }
                    .disabled(model.unmountedObjects.isEmpty || model.isBusy)

                    Spacer()
                    Text("Image size: \(ByteCount.format(image.physicalSize))")
                        .foregroundStyle(.secondary)
                }
            }
        } else {
            VStack(spacing: 12) {
                Image(systemName: "doc.badge.plus")
                    .font(.system(size: 36))
                    .foregroundStyle(.secondary)
                Text("Open an RDR disk image")
                    .font(.title3.weight(.medium))
                Text("Choose a file, drag it here, or double-click an .rdr file in Finder.")
                    .foregroundStyle(.secondary)
                Button("Choose Image") { model.chooseImage() }
                    .buttonStyle(.borderedProminent)
            }
            .frame(maxWidth: .infinity, minHeight: 180)
        }
    }

    private func objectRow(_ object: RDRObject) -> some View {
        let session = model.session(for: object.id)
        return Button {
            model.selectedObjectID = object.id
        } label: {
            HStack(spacing: 12) {
                Image(systemName: model.selectedObjectID == object.id ? "checkmark.circle.fill" : "circle")
                    .foregroundStyle(model.selectedObjectID == object.id ? Color.accentColor : .secondary)
                Image(systemName: session == nil ? "internaldrive" : "externaldrive.fill.badge.checkmark")
                    .font(.title3)
                    .foregroundStyle(session == nil ? Color.primary : .green)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 7) {
                        Text("Disk object \(object.id)")
                            .fontWeight(.medium)
                        if session != nil {
                            Text("MOUNTED")
                                .font(.caption2.weight(.bold))
                                .foregroundStyle(.green)
                        }
                    }
                    Text(objectDetail(object, session: session))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Text(ByteCount.format(object.logicalSize))
                    .font(.body.monospacedDigit())
            }
            .contentShape(Rectangle())
            .padding(12)
        }
        .buttonStyle(.plain)
    }

    private var sessionsSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            sectionTitle("Mounted RDR disks", detail: model.sessions.isEmpty ? "None" : "\(model.sessions.count) sessions")
            if model.sessions.isEmpty {
                Text("No RDR disk objects are mounted.")
                    .foregroundStyle(.secondary)
                    .padding(.vertical, 8)
            } else {
                if !model.currentImageSessions.isEmpty {
                    Text("FROM THIS IMAGE")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.secondary)
                    ForEach(model.currentImageSessions) { session in
                        sessionCard(session, belongsToCurrentImage: true)
                    }
                }
                if !model.otherImageSessions.isEmpty {
                    Text(model.imageURL == nil ? "ALL IMAGES" : "FROM OTHER IMAGES")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.secondary)
                    ForEach(model.otherImageSessions) { session in
                        sessionCard(session, belongsToCurrentImage: false)
                    }
                }
            }
        }
    }

    private func sessionCard(_ session: MountSession, belongsToCurrentImage: Bool) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(systemName: "externaldrive.fill.badge.checkmark")
                    .foregroundStyle(.green)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Disk object \(session.object) from \(URL(fileURLWithPath: session.image).lastPathComponent)")
                        .fontWeight(.medium)
                    Text(session.image)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
                Button("Unmount", role: .destructive) {
                    Task { await model.unmount(session) }
                }
                .disabled(model.isBusy)
            }
            LabeledContent("Device", value: session.device ?? "Not attached")
                .font(.caption.monospaced())
            LabeledContent("Session", value: session.sessionID)
                .font(.caption.monospaced())
            ForEach(session.volumes, id: \.self) { volume in
                HStack {
                    Image(systemName: "folder")
                    VStack(alignment: .leading, spacing: 2) {
                        Text(URL(fileURLWithPath: volume.mountPoint).lastPathComponent)
                            .fontWeight(.medium)
                        Text(volume.mountPoint)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                    }
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    Button("Open in Finder") { model.openInFinder(volume) }
                }
                .padding(.leading, 28)
            }
        }
        .padding(14)
        .background(
            belongsToCurrentImage ? Color.accentColor.opacity(0.08) : Color(nsColor: .controlBackgroundColor),
            in: RoundedRectangle(cornerRadius: 10)
        )
    }

    private func objectDetail(_ object: RDRObject, session: MountSession?) -> String {
        guard let session else {
            return "Not mounted, \(object.chunks) chunks"
        }
        let locations = session.volumes.map(\.mountPoint)
        let destination = locations.isEmpty ? (session.device ?? session.state) : locations.joined(separator: ", ")
        return "\(session.device ?? session.state) at \(destination)"
    }

    private func sectionTitle(_ title: String, detail: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title).font(.headline)
            Spacer()
            Text(detail).font(.subheadline).foregroundStyle(.secondary)
        }
    }

    private var statusBar: some View {
        HStack(spacing: 10) {
            if model.isBusy { ProgressView().controlSize(.small) }
            Text(model.statusMessage ?? "Ready")
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 9)
    }
}
