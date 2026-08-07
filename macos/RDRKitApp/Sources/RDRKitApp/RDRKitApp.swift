import AppKit
import SwiftUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    func application(_ sender: NSApplication, openFiles filenames: [String]) {
        guard let filename = filenames.first else {
            sender.reply(toOpenOrPrint: .failure)
            return
        }

        sender.reply(toOpenOrPrint: .success)
        Task { @MainActor in
            await AppModel.shared.openImage(URL(fileURLWithPath: filename))
        }
    }
}

@main
struct RDRKitApplication: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = AppModel.shared

    var body: some Scene {
        Window("RDRKit", id: "main") {
            ContentView(model: model)
        }
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("Open Disk Image...") { model.chooseImage() }
                    .keyboardShortcut("o", modifiers: .command)
            }
        }
    }
}
