import AVFoundation
import UIKit

/// How long the microphone stays armed after the last take.
enum ReadyFor: String, CaseIterable, Identifiable {
    case fiveMinutes, fifteenMinutes, oneHour, untilOff

    var id: String { rawValue }

    var seconds: TimeInterval? {
        switch self {
        case .fiveMinutes: return 5 * 60
        case .fifteenMinutes: return 15 * 60
        case .oneHour: return 60 * 60
        case .untilOff: return nil
        }
    }

    var label: String {
        switch self {
        case .fiveMinutes: return "5 minutes"
        case .fifteenMinutes: return "15 minutes"
        case .oneHour: return "1 hour"
        case .untilOff: return "Until I turn it off"
        }
    }
}

/// The one owner of the microphone, the engine and `state.json`. The keyboard
/// reads that file and sends signals. It never decides anything here.
@MainActor
final class Session: ObservableObject {
    @Published private(set) var state = SessionState()
    @Published private(set) var stage: Engine.Stage = .waiting
    @Published private(set) var recent: [History.Entry] = History.recent(limit: 30)
    /// The app was opened by the keyboard and could not send the user back.
    @Published var showSwipeBack = false
    @Published var readyFor: ReadyFor {
        didSet {
            UserDefaults.standard.set(readyFor.rawValue, forKey: "readyFor")
            scheduleDisarm()
        }
    }

    private let microphone = Microphone()
    private let engine = Engine.shared
    private var micIsOn = false
    /// Why the microphone went away during a take, shown once that take is done.
    private var job: UInt64 = 0
    /// The short wait for the last audio block. Cancel can still stop it.
    private var pendingFinish: DispatchWorkItem?
    private var disarmAt: DispatchWorkItem?
    /// iOS can suspend the app once a call takes the microphone, mid-transcription.
    private var takeTask = UIBackgroundTaskIdentifier.invalid
    private var lastPing = Date.distantPast
    private var keyboardWatch: Timer?
    /// The keyboard hid mid-take on purpose, so its silence is not a crash.
    private var keyboardAway = false
    /// The user sent the live words, so the finished take must not be typed again.
    private var takeSent = false
    /// Tells a tap from this app's own field apart from one that launched or resumed it.
    private var hasBeenInFront = false
    /// For the diagnostics log only.
    private var takeStarted = Date()
    private var takeAudio = CaptureStats()
    private var finishAsked = Date()

    /// A keyboard that stops pinging has gone: the user left, or it was killed.
    private static let keyboardGoneAfter: TimeInterval = 3
    /// A take the user walked away from ends after this, in case they forgot it.
    private static let awayLimit: TimeInterval = 5 * 60

    init(benching: Bool) {
        readyFor =
            UserDefaults.standard.string(forKey: "readyFor").flatMap(ReadyFor.init(rawValue:))
            ?? .oneHour
        var restored = SessionState()
        restored.take = Bridge.read(SessionState.self, "state.json")?.take
        state = restored
        publish()
        guard !benching else { return }
        Diagnostics.log("app_launch", ["phase": state.phase.rawValue])

        engine.onStage = { [weak self] stage in
            MainActor.assumeIsolated { self?.stageChanged(stage) }
        }
        engine.onDone = { [weak self] job, result in
            MainActor.assumeIsolated { self?.finished(job: job, result) }
        }
        engine.onPreview = { [weak self] words in
            MainActor.assumeIsolated { self?.previewed(words) }
        }
        engine.start(previews: true)

        Signals.observe(.ping) { [weak self] in
            MainActor.assumeIsolated { self?.lastPing = Date() }
            Signals.post(.pong)
        }
        Signals.observe(.start) { [weak self] in MainActor.assumeIsolated { self?.startTake() } }
        Signals.observe(.stop) { [weak self] in MainActor.assumeIsolated { self?.stopTake() } }
        Signals.observe(.cancel) { [weak self] in MainActor.assumeIsolated { self?.cancelTake() } }
        Signals.observe(.away) { [weak self] in MainActor.assumeIsolated { self?.keyboardLeft() } }
        Signals.observe(.sent) { [weak self] in MainActor.assumeIsolated { self?.fieldSent() } }

        let center = NotificationCenter.default
        center.addObserver(
            forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            Diagnostics.log("app_active")
            MainActor.assumeIsolated { self?.hasBeenInFront = true }
        }
        center.addObserver(
            forName: UIApplication.willResignActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.hasBeenInFront = false }
        }
        center.addObserver(
            forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
        ) { [weak self] _ in
            Diagnostics.log("app_background")
            MainActor.assumeIsolated { self?.showSwipeBack = false }
        }
        center.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(), queue: .main
        ) { [weak self] note in
            let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            let began = raw.flatMap(AVAudioSession.InterruptionType.init(rawValue:)) == .began
            let reason = note.userInfo?[AVAudioSessionInterruptionReasonKey] as? UInt
            MainActor.assumeIsolated {
                Diagnostics.log(
                    "interruption",
                    [
                        "type": began ? "began" : "ended", "reason": reason.map(String.init) ?? "none",
                        "phase": self?.state.phase.rawValue ?? "none",
                    ])
                if began { self?.micLost(because: "interruption") }
            }
        }
        center.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.routeChanged() }
        }
        center.addObserver(
            forName: AVAudioSession.mediaServicesWereResetNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                Diagnostics.log("media_services_reset")
                self?.micLost(because: "media_reset")
                self?.microphone.rebuild()
            }
        }
    }

    // MARK: Arming. iOS only lets the microphone start while the app is in front.

    /// Idempotent, because every path into the app calls it.
    func arm() async {
        guard !micIsOn else { return }
        guard await AVAudioApplication.requestRecordPermission() else {
            Diagnostics.log("arm_denied")
            state.problem = "Microphone access is off."
            publish()
            return
        }
        do {
            try microphone.start()
        } catch {
            Diagnostics.log("arm_failed", ["error": error])
            state.problem = "The microphone would not start."
            publish()
            return
        }
        micIsOn = true
        Diagnostics.log("armed", ["phase": state.phase.rawValue])
        state.problem = nil
        if state.phase == .off { state.phase = .armed }
        publish()
        scheduleDisarm()
    }

    /// Drops a take still being recorded. One already being written finishes, so
    /// its words are not lost.
    func turnOff() {
        disarmAt?.cancel()
        Diagnostics.log("mic_off", ["phase": state.phase.rawValue])
        if state.phase == .recording {
            Diagnostics.log("take_dropped", ["seconds": Date().timeIntervalSince(takeStarted)])
            stopWatchingKeyboard()
            microphone.endCapture()
            engine.discard()
            state.phase = .off
        }
        microphone.stop()
        micIsOn = false
        if state.phase != .transcribing {
            state.phase = .off
            state.problem = nil
        }
        publish()
    }

    func retryEngine() {
        Diagnostics.log("engine_retry")
        stage = .waiting
        engine.restart(previews: true)
    }

    /// A keyboard tap brought the app forward. Arm, then go straight back, unless
    /// the model still has to download or the tap came from this app's own field.
    func openedFromKeyboard() {
        let wasInFront = UIApplication.shared.applicationState == .active && hasBeenInFront
        Task {
            await arm()
            Diagnostics.log("opened_from_keyboard", ["in_front": wasInFront, "phase": state.phase.rawValue])
            guard micIsOn, Engine.isModelPrepared else {
                Diagnostics.log("handoff_not_ready", ["mic": micIsOn, "model": Engine.isModelPrepared])
                return
            }
            if wasInFront {
                Diagnostics.log("handoff_in_front")
                Bridge.remove("handoff.json")
                startTake()
                return
            }
            let host = Bridge.read(Handoff.self, "handoff.json")?.host
            if let host, HostReturn.open(bundle: host) {
                Diagnostics.log("host_return", ["host": host])
                return
            }
            Diagnostics.log("swipe_back", ["host": host ?? "none"])
            showSwipeBack = true
        }
    }

    private func scheduleDisarm() {
        disarmAt?.cancel()
        guard state.phase == .armed, let seconds = readyFor.seconds else { return }
        let work = DispatchWorkItem { [weak self] in
            Diagnostics.log("disarm_timer")
            self?.turnOff()
        }
        disarmAt = work
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds, execute: work)
    }

    // MARK: Takes

    private func startTake() {
        guard state.phase == .armed else {
            Diagnostics.log("start_ignored", ["phase": state.phase.rawValue])
            return
        }
        if case .failed(let message) = stage {
            Diagnostics.log("start_refused", ["error": message])
            state.problem = message
            publish()
            return
        }
        disarmAt?.cancel()
        Bridge.remove("words.json")
        keyboardAway = false
        takeSent = false
        // iOS can reclaim the memory-mapped weights while the app sits idle. As on
        // the Mac, page them back in during the take rather than after it.
        engine.warm()
        engine.begin()
        let engine = self.engine
        let audio = CaptureStats()
        takeAudio = audio
        takeStarted = Date()
        Diagnostics.log("take_started", ["stage": stage])
        microphone.beginCapture {
            audio.add($0)
            engine.feed($0)
        }
        state.phase = .recording
        state.problem = nil
        state.since = Date()
        publish()
        watchKeyboard()
    }

    /// The take so far, for the keyboard to show. A late pass changes nothing once
    /// the take has ended.
    private func previewed(_ words: String) {
        guard state.phase == .recording else { return }
        Bridge.write(words, "words.json")
        Signals.post(.words)
    }

    private func keyboardLeft() {
        guard state.phase == .recording else { return }
        Diagnostics.log("keyboard_away")
        keyboardAway = true
    }

    private func fieldSent() {
        Diagnostics.log("field_sent", ["phase": state.phase.rawValue])
        switch state.phase {
        case .recording:
            takeSent = true
            stopTake(by: "sent")
        case .transcribing:
            takeSent = true
        default:
            break
        }
    }

    private func stopTake(by cause: String = "keyboard") {
        guard state.phase == .recording else {
            Diagnostics.log("stop_ignored", ["phase": state.phase.rawValue, "by": cause])
            return
        }
        let seconds = Date().timeIntervalSince(takeStarted)
        stopWatchingKeyboard()
        state.phase = .transcribing
        publish()
        takeTask = UIApplication.shared.beginBackgroundTask { [weak self] in
            MainActor.assumeIsolated {
                Diagnostics.log("background_expired", ["phase": self?.state.phase.rawValue ?? "none"])
                self?.endTakeTask()
            }
        }
        // The tap delivers audio in blocks, so the last word is still in one.
        let work = DispatchWorkItem { [self] in
            pendingFinish = nil
            microphone.endCapture()
            let audio = takeAudio.summary()
            Diagnostics.log(
                "take_stopped",
                [
                    "by": cause, "seconds": seconds, "audio_s": audio.seconds,
                    "peak_db": audio.peak, "rms_db": audio.rms,
                ])
            finishAsked = Date()
            job = engine.finish()
            if job == 0 {
                Diagnostics.log("engine_finish_failed")
                settle(problem: "The speech engine stopped.")
            }
        }
        pendingFinish = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: work)
    }

    private func cancelTake() {
        Diagnostics.log("take_cancelled", ["phase": state.phase.rawValue])
        switch state.phase {
        case .recording:
            stopWatchingKeyboard()
            microphone.endCapture()
            engine.discard()
        case .transcribing:
            if let pending = pendingFinish {
                pending.cancel()
                pendingFinish = nil
                microphone.endCapture()
                engine.discard()
            }
            // A transcript still in flight now matches no job and lands nowhere.
            job = 0
        default:
            return
        }
        settle(problem: nil)
    }

    private func finished(job finishedJob: UInt64, _ result: Result<String, EngineFailure>) {
        guard finishedJob == job else {
            Diagnostics.log("transcript_stale", ["job": finishedJob, "current": job])
            return
        }
        job = 0
        let engineMs = Int(Date().timeIntervalSince(finishAsked) * 1000)
        switch result {
        case .success(let text):
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty {
                let audio = takeAudio.summary()
                Diagnostics.log(
                    "transcript_empty",
                    ["engine_ms": engineMs, "audio_s": audio.seconds, "peak_db": audio.peak, "rms_db": audio.rms])
                settle(problem: nil)
                return
            }
            Diagnostics.log(
                "transcript_delivered",
                ["words": trimmed.split(whereSeparator: \.isWhitespace).count, "chars": trimmed.count, "engine_ms": engineMs])
            let now = Date()
            state.take = Take(
                id: Int64(now.timeIntervalSince1970 * 1000), text: trimmed, finished: now, sent: takeSent ? true : nil)
            History.append(trimmed, at: now)
            recent = History.recent(limit: 30)
            settle(problem: nil)
        case .failure(let failure):
            Diagnostics.log("engine_error", ["error": failure.message, "engine_ms": engineMs])
            settle(problem: failure.message)
        }
    }

    /// Every way a take can end comes through here, so none can leave the state
    /// saying the mic is ready when it is not.
    private func settle(problem: String?) {
        endTakeTask()
        state.phase = micIsOn ? .armed : .off
        state.problem = problem
        publish()
        scheduleDisarm()
    }

    private func endTakeTask() {
        guard takeTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(takeTask)
        takeTask = .invalid
    }

    /// Nothing else ends a tapped take if the keyboard process dies or is
    /// suspended, and the microphone would keep recording what comes next.
    private func watchKeyboard() {
        lastPing = Date()
        keyboardWatch = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                let limit = self.keyboardAway ? Self.awayLimit : Self.keyboardGoneAfter
                guard Date().timeIntervalSince(self.lastPing) > limit else { return }
                Diagnostics.log(
                    "keyboard_gone", ["since_ping_s": Date().timeIntervalSince(self.lastPing), "away": self.keyboardAway])
                self.stopTake(by: "keyboard_gone")
            }
        }
    }

    private func stopWatchingKeyboard() {
        keyboardWatch?.invalidate()
        keyboardWatch = nil
    }

    // MARK: The system taking the microphone away, or the engine failing

    /// Says nothing, because the next tap brings the mic back.
    private func micLost(because cause: String) {
        Diagnostics.log("mic_lost", ["by": cause, "mic": micIsOn, "phase": state.phase.rawValue])
        guard micIsOn else { return }
        if state.phase == .recording { stopTake(by: "mic_lost") }
        turnOff()
    }

    private func routeChanged() {
        Diagnostics.log("route_change", ["mic": micIsOn, "phase": state.phase.rawValue])
        guard micIsOn else { return }
        do {
            try microphone.restart()
        } catch {
            Diagnostics.log("route_restart_failed", ["error": error])
            micLost(because: "route")
        }
    }

    private func stageChanged(_ newStage: Engine.Stage) {
        switch (stage, newStage) {
        case (.downloading, .downloading), (_, .failed): break
        default: Diagnostics.log("engine_stage", ["stage": newStage])
        }
        stage = newStage
        guard case .failed(let message) = newStage else { return }
        Diagnostics.log("engine_failed", ["error": message, "phase": state.phase.rawValue])
        switch state.phase {
        case .recording:
            stopWatchingKeyboard()
            microphone.endCapture()
        case .transcribing:
            pendingFinish?.cancel()
            pendingFinish = nil
            microphone.endCapture()
            job = 0
        default:
            return
        }
        settle(problem: message)
    }

    private func publish() {
        Bridge.write(state, "state.json")
        Signals.post(.changed)
    }
}
