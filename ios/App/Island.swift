import ActivityKit
import Foundation

/// Keeps the Live Activity in step with the session. iOS lets an app start one
/// only while it is in front, so it starts when the mic is armed and lives until
/// the mic turns off. Takes only update it, from the background.
@MainActor
enum Island {
    private static var activity: Activity<TakeActivity>?
    private static var shown: TakeActivity.ContentState?
    /// What the session last said, before the meter adds its levels.
    private static var base: TakeActivity.ContentState?
    private static var meter: Timer?
    private static var heard: [Int] = []
    private static var framesSent = 0
    private static var framesLanded = 0

    /// Samples a second of the mic level in five steps, then sends them as one frame.
    private static let samplesPerFrame = 5

    /// An activity outlives the app process that started it.
    static func endLeftovers() {
        for old in Activity<TakeActivity>.activities {
            Task { await old.end(nil, dismissalPolicy: .immediate) }
        }
    }

    static func show(_ state: SessionState) {
        let content = content(for: state)
        guard content != base else { return }
        base = content
        content?.phase == .recording ? startMeter() : stopMeter()
        send(content)
    }

    private static func send(_ content: TakeActivity.ContentState?) {
        guard let content else {
            end()
            return
        }
        guard content != shown else { return }
        if let activity, activity.activityState == .active {
            shown = content
            Task { await update(activity, to: content) }
            return
        }
        if let activity {
            Diagnostics.log("island_gone", ["state": "\(activity.activityState)"])
        }
        guard ActivityAuthorizationInfo().areActivitiesEnabled else {
            Diagnostics.log("island_disabled")
            return
        }
        do {
            activity = try Activity.request(
                attributes: TakeActivity(), content: ActivityContent(state: content, staleDate: nil))
            shown = content
            Diagnostics.log("island_started", ["phase": content.phase.rawValue])
        } catch {
            Diagnostics.log("island_failed", ["phase": content.phase.rawValue, "error": error])
        }
    }

    /// iOS drops an update that comes too soon after the activity started, so this
    /// tries again until it lands or a newer state replaces it.
    private static func update(_ activity: Activity<TakeActivity>, to content: TakeActivity.ContentState) async {
        for attempt in 1...5 {
            guard shown == content, activity.activityState == .active else { return }
            await activity.update(ActivityContent(state: content, staleDate: nil))
            let now = activity.content.state
            let isFrame = content.levels != nil && attempt == 1
            if isFrame { framesSent += 1 }
            if now == content, isFrame { framesLanded += 1 }
            if !isFrame {
                Diagnostics.log(
                    "island_updated", ["asked": content.phase.rawValue, "now": now.phase.rawValue, "attempt": attempt])
            }
            if now == content || isFrame { return }
            try? await Task.sleep(nanoseconds: 700_000_000)
        }
    }

    private static func startMeter() {
        guard meter == nil else { return }
        heard = []
        framesSent = 0
        framesLanded = 0
        let timer = Timer(timeInterval: 0.2, repeats: true) { _ in
            MainActor.assumeIsolated { listen() }
        }
        RunLoop.main.add(timer, forMode: .common)
        meter = timer
    }

    private static func stopMeter() {
        guard let meter else { return }
        meter.invalidate()
        self.meter = nil
        Diagnostics.log("island_meter", ["sent": framesSent, "landed": framesLanded])
    }

    private static func listen() {
        heard.append(Int((Bridge.readLevel() * 10).rounded()))
        guard heard.count >= samplesPerFrame, var content = base, content.phase == .recording else { return }
        content.levels = heard
        heard = []
        send(content)
    }

    private static func end() {
        guard let activity else { return }
        self.activity = nil
        shown = nil
        Task { await activity.end(nil, dismissalPolicy: .immediate) }
    }

    private static func content(for state: SessionState) -> TakeActivity.ContentState? {
        switch state.phase {
        case .off: return nil
        case .armed: return .init(phase: .ready)
        case .recording: return .init(phase: .recording, since: state.since)
        case .transcribing: return .init(phase: .writing)
        }
    }
}
