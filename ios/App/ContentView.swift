import AVFAudio
import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct ContentView: View {
    @EnvironmentObject private var session: Session
    @Environment(\.scenePhase) private var scenePhase
    @State private var tryIt = ""
    @State private var pickingFolder = false
    @State private var termCount = DictionaryFolder.termCount()
    @State private var connected = DictionaryFolder.isConnected
    @State private var copied: Date?
    @FocusState private var tryItFocused: Bool

    var body: some View {
        NavigationStack {
            List {
                Section { status }
                Section {
                    TextField("Try it. Switch to the see.computer keyboard here.", text: $tryIt, axis: .vertical)
                        .focused($tryItFocused)
                        .lineLimit(2...6)
                }
                setup
                settings
                history
            }
            .navigationTitle("see.computer")
        }
        .fullScreenCover(isPresented: $session.showSwipeBack) { SwipeBackView() }
        .fileImporter(isPresented: $pickingFolder, allowedContentTypes: [.folder]) { result in
            guard case .success(let url) = result else { return }
            DictionaryFolder.connect(url)
            connected = true
            refreshDictionary()
        }
        .onChange(of: scenePhase) { _, phase in
            guard phase == .active else { return }
            refreshDictionary()
        }
        .onChange(of: session.stage) { _, stage in
            // Auto-lock would suspend the app and kill the download.
            if case .downloading = stage {
                UIApplication.shared.isIdleTimerDisabled = true
            } else {
                UIApplication.shared.isIdleTimerDisabled = false
            }
        }
        .task { refreshDictionary() }
    }

    private func refreshDictionary() {
        DispatchQueue.global(qos: .utility).async {
            DictionaryFolder.sync()
            let count = DictionaryFolder.termCount()
            DispatchQueue.main.async { termCount = count }
        }
    }

    @ViewBuilder private var status: some View {
        switch session.stage {
        case .downloading(let percent):
            Label("Downloading the speech model, \(percent)%. Keep this screen open.", systemImage: "arrow.down.circle")
        case .failed(let message):
            VStack(alignment: .leading, spacing: 8) {
                Label(message, systemImage: "exclamationmark.triangle")
                Button("Try again") { session.retryEngine() }
            }
        case .waiting, .loading:
            Label("Loading the speech model", systemImage: "hourglass")
        case .ready:
            EmptyView()
        }
        if session.state.phase == .off {
            HStack {
                Label(session.state.problem ?? "Microphone off", systemImage: "mic.slash")
                Spacer()
                if AVAudioApplication.shared.recordPermission == .denied {
                    Button("Settings") {
                        UIApplication.shared.open(URL(string: UIApplication.openSettingsURLString)!)
                    }
                } else {
                    Button("Turn on") { Task { await session.arm() } }
                }
            }
        } else {
            HStack {
                Label("Microphone ready", systemImage: "mic.fill")
                Spacer()
                Button("Turn off") { session.turnOff() }
            }
        }
    }

    private var setup: some View {
        Section("Keyboard") {
            Button("Add the keyboard and allow Full Access") {
                UIApplication.shared.open(URL(string: UIApplication.openSettingsURLString)!)
            }
            Text("In Settings, open Keyboards, turn on see.computer, then turn on Allow Full Access. The keyboard needs it to talk to this app. Audio and text stay on this iPhone.")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }

    private var settings: some View {
        Section("Settings") {
            Picker("Keep the mic ready for", selection: $session.readyFor) {
                ForEach(ReadyFor.allCases) { Text($0.label).tag($0) }
            }
            if connected {
                LabeledContent("Dictionary", value: "\(termCount) words")
                Button("Disconnect the dictionary folder", role: .destructive) {
                    DictionaryFolder.disconnect()
                    connected = false
                }
            } else {
                Button("Use the dictionary from your Mac") { pickingFolder = true }
                Text("Choose iCloud Drive, Documents, see.computer.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var history: some View {
        if !session.recent.isEmpty {
            Section("Recent") {
                ForEach(session.recent) { entry in
                    Button {
                        UIPasteboard.general.string = entry.text
                        copied = entry.at
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(entry.text).foregroundStyle(.primary)
                            Text(copied == entry.at ? "Copied" : entry.at.formatted(date: .abbreviated, time: .shortened))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
    }
}

/// Shown when the keyboard opened the app and the app could not send the user back.
struct SwipeBackView: View {
    @EnvironmentObject private var session: Session
    @State private var nudge = false

    var body: some View {
        VStack(spacing: 20) {
            Spacer()
            Image(systemName: "mic.fill")
                .font(.system(size: 44))
                .foregroundStyle(Color.accentColor)
            Text("Swipe right along the bottom edge to go back")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 32)
            Spacer()
            Image(systemName: "arrow.right")
                .font(.title.weight(.medium))
                .foregroundStyle(.secondary)
                .offset(x: nudge ? 28 : -28)
                .animation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true), value: nudge)
                .padding(.bottom, 24)
        }
        .contentShape(Rectangle())
        .onAppear { nudge = true }
        .onTapGesture { session.showSwipeBack = false }
    }
}
