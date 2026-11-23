import AVFAudio
import SwiftUI
import UIKit

struct ContentView: View {
    @ObservedObject var stats: StatsStore
    @EnvironmentObject private var session: Session
    @Environment(\.scenePhase) private var scenePhase
    @State private var showingSettings = false
    @State private var copied: Date?

    var body: some View {
        NavigationStack {
            List {
                if stats.value.total > 0 {
                    Section {
                        StatsCell(store: stats)
                            .listRowInsets(EdgeInsets(top: 0, leading: 16, bottom: 0, trailing: 16))
                    }
                }
                if needsAttention {
                    Section { status }
                }
                history
            }
            .navigationTitle("see.computer")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { micButton }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showingSettings = true
                    } label: {
                        Image(systemName: "gearshape")
                    }
                    .accessibilityLabel("Settings")
                }
            }
            .overlay(alignment: .bottom) {
                if copied != nil {
                    CopiedToast()
                        .padding(.bottom, 12)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
            }
        }
        .sheet(isPresented: $showingSettings) { SettingsView() }
        .sensoryFeedback(trigger: copied) { _, new in new == nil ? nil : .success }
        .task(id: copied) {
            guard copied != nil else { return }
            try? await Task.sleep(for: .seconds(1.6))
            guard !Task.isCancelled else { return }
            withAnimation(.smooth) { copied = nil }
        }
        .fullScreenCover(isPresented: $session.showSwipeBack) { SwipeBackView() }
        .onChange(of: scenePhase) { _, phase in
            guard phase == .active else { return }
            syncDictionary()
        }
        .onChange(of: session.stage) { _, stage in
            // Auto-lock would suspend the app and kill the download.
            if case .downloading = stage {
                UIApplication.shared.isIdleTimerDisabled = true
            } else {
                UIApplication.shared.isIdleTimerDisabled = false
            }
        }
        .task { syncDictionary() }
    }

    private func syncDictionary() {
        DispatchQueue.global(qos: .utility).async { DictionaryFolder.sync() }
        session.stats.refresh()
    }

    private var micIsOff: Bool { session.state.phase == .off }
    private var micDenied: Bool { AVAudioApplication.shared.recordPermission == .denied }

    private var micNeedsAttention: Bool { micIsOff && (session.state.problem != nil || micDenied) }

    private var needsAttention: Bool {
        if case .ready = session.stage { return micNeedsAttention }
        return true
    }

    private var micButton: some View {
        Button {
            if micIsOff {
                Task { await session.arm() }
            } else {
                session.turnOff()
            }
        } label: {
            Image(systemName: micIsOff ? "mic.slash" : "mic.fill")
                .contentTransition(.symbolEffect(.replace))
        }
        .tint(micIsOff ? .secondary : .accentColor)
        .accessibilityLabel(micIsOff ? "Turn on the microphone" : "Turn off the microphone")
    }

    @ViewBuilder private var status: some View {
        switch session.stage {
        case .downloading(let percent):
            ProgressView(value: Double(percent), total: 100) {
                Text("Downloading the speech model")
            } currentValueLabel: {
                Text("\(percent)%. Keep this screen open.")
            }
            .padding(.vertical, 4)
        case .failed(let message):
            VStack(alignment: .leading, spacing: 8) {
                Label(message, systemImage: "exclamationmark.triangle")
                Button("Try again") { session.retryEngine() }
            }
        case .waiting, .loading:
            Label {
                Text("Loading the speech model")
            } icon: {
                ProgressView()
            }
        case .ready:
            EmptyView()
        }
        if micNeedsAttention {
            HStack {
                Label(session.state.problem ?? "Microphone access is off.", systemImage: "mic.slash")
                Spacer()
                if micDenied {
                    Button("Settings") {
                        UIApplication.shared.open(URL(string: UIApplication.openSettingsURLString)!)
                    }
                } else {
                    Button("Turn on") { Task { await session.arm() } }
                }
            }
        }
    }

    @ViewBuilder private var history: some View {
        if session.recent.isEmpty {
            Section {
                ContentUnavailableView(
                    "No dictations yet",
                    systemImage: "waveform",
                    description: Text("Switch to the see.computer keyboard in any app and tap Dictate.")
                )
            }
        } else {
            Section("Recent") {
                ForEach(session.recent) { entry in
                    Button {
                        UIPasteboard.general.string = entry.text
                        withAnimation(.smooth) { copied = Date() }
                    } label: {
                        // A button's hierarchical styles resolve to its tint, so the
                        // colors here are concrete.
                        VStack(alignment: .leading, spacing: 6) {
                            Text(entry.text)
                                .foregroundStyle(Color.primary)
                                .multilineTextAlignment(.leading)
                            Text(Self.stamp(entry.at))
                                .font(.footnote)
                                .foregroundStyle(Color.secondary)
                        }
                        .padding(.vertical, 4)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .contextMenu {
                        Button("Copy", systemImage: "doc.on.doc") {
                            UIPasteboard.general.string = entry.text
                            withAnimation(.smooth) { copied = Date() }
                        }
                        ShareLink(item: entry.text)
                    }
                }
            }
        }
    }

    private static func stamp(_ date: Date) -> String {
        let time = date.formatted(date: .omitted, time: .shortened)
        let calendar = Calendar.current
        if calendar.isDateInToday(date) { return time }
        if calendar.isDateInYesterday(date) { return "Yesterday, \(time)" }
        if calendar.isDate(date, equalTo: Date(), toGranularity: .year) {
            return "\(date.formatted(.dateTime.month(.abbreviated).day())), \(time)"
        }
        return date.formatted(date: .abbreviated, time: .shortened)
    }
}

private struct CopiedToast: View {
    var body: some View {
        Label("Copied", systemImage: "checkmark.circle.fill")
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Color.primary)
            .padding(.horizontal, 18)
            .padding(.vertical, 12)
            .glassCapsule()
    }
}

private extension View {
    @ViewBuilder func glassCapsule() -> some View {
        if #available(iOS 26, *) {
            glassEffect(.regular, in: .capsule)
        } else {
            background(.regularMaterial, in: Capsule())
                .shadow(color: .black.opacity(0.12), radius: 12, y: 4)
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
