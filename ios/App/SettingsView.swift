import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct SettingsView: View {
    @EnvironmentObject private var session: Session
    @Environment(\.dismiss) private var dismiss
    @State private var tryIt = ""
    @State private var pickingFolder = false
    @State private var termCount = 0
    @State private var connected = DictionaryFolder.isConnected
    @AppStorage(TypingLanguages.key, store: TypingLanguages.store) private var typingLanguages = TypingLanguages.device
    @AppStorage(TypingLog.key, store: TypingLog.store) private var keepsTypingLog = true

    private var hasNoDictations: Bool { session.recent.isEmpty }

    var body: some View {
        NavigationStack {
            Form {
                if hasNoDictations {
                    Section {
                        Button("Add the keyboard and allow Full Access") {
                            UIApplication.shared.open(URL(string: UIApplication.openSettingsURLString)!)
                        }
                        TextField("Then try it here.", text: $tryIt, axis: .vertical)
                            .lineLimit(2...6)
                    } header: {
                        Text("Set up the keyboard")
                    } footer: {
                        Text("In Settings, open Keyboards, turn on see.computer, then turn on Allow Full Access. The keyboard needs it to talk to this app. Audio and text stay on this iPhone.")
                    }
                }

                Section {
                    Picker("Keep the mic ready for", selection: $session.readyFor) {
                        ForEach(ReadyFor.allCases) { Text($0.label).tag($0) }
                    }
                }

                Section {
                    if connected {
                        LabeledContent("Words", value: termCount.formatted())
                        Button("Disconnect the dictionary folder", role: .destructive) {
                            DictionaryFolder.disconnect()
                            connected = false
                            session.stats.refresh()
                        }
                    } else {
                        Button("Use the dictionary from your Mac") { pickingFolder = true }
                    }
                } header: {
                    Text("Dictionary")
                } footer: {
                    if !connected { Text("Choose iCloud Drive, Documents, see.computer.") }
                }

                Section {
                    Picker("Languages", selection: $typingLanguages) {
                        ForEach(TypingLanguages.allCases) { Text($0.label).tag($0) }
                    }
                    .onChange(of: typingLanguages) { _, _ in Signals.post(.typingLanguagesChanged) }
                    Toggle("Keep a typing log", isOn: $keepsTypingLog)
                        .onChange(of: keepsTypingLog) { _, on in if !on { TypingLog.erase() } }
                } header: {
                    Text("Keyboard")
                } footer: {
                    Text("The typing log keeps each key, swipe and correction on this iPhone so mistyped words can be studied later. Turning it off deletes it. Swipe typing uses FUTO Swipe.")
                }
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) { doneButton }
            }
        }
        .fileImporter(isPresented: $pickingFolder, allowedContentTypes: [.folder]) { result in
            guard case .success(let url) = result else { return }
            DictionaryFolder.connect(url)
            connected = true
            countTerms(syncing: true)
            session.stats.refresh()
        }
        .task { countTerms(syncing: false) }
    }

    @ViewBuilder private var doneButton: some View {
        if #available(iOS 26, *) {
            Button(role: .close) { dismiss() }
        } else {
            Button("Done") { dismiss() }.fontWeight(.semibold)
        }
    }

    private func countTerms(syncing: Bool) {
        DispatchQueue.global(qos: .utility).async {
            if syncing { DictionaryFolder.sync() }
            let count = DictionaryFolder.termCount()
            DispatchQueue.main.async { termCount = count }
        }
    }
}
