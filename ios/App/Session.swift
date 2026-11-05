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
    /// Tells a tap from this app's own field apart from one that launched or resumed it.
    private var hasBeenInFront = false

    /// A keyboard that stops pinging has gone: the user left, or it was killed.
    private static let keyboardGoneAfter: TimeInterval = 3

    init(benching: Bool) {
        readyFor =
            UserDefaults.standard.string(forKey: "readyFor").flatMap(ReadyFor.init(rawValue:))
            ?? .oneHour
        var restored = SessionState()
        restored.take = Bridge.read(SessionState.self, "state.json")?.take
        state = restored
        publish()
        guard !benching else { return }

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

        let center = NotificationCenter.default
        center.addObserver(
            forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
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
            MainActor.assumeIsolated { self?.showSwipeBack = false }
        }
        center.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(), queue: .main
        ) { [weak self] note in
            let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            let began = raw.flatMap(AVAudioSession.InterruptionType.init(rawValue:)) == .began
            MainActor.assumeIsolated {
                if began { self?.micLost() }
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
                self?.micLost()
                self?.microphone.rebuild()
            }
        }
    }

    // MARK: Arming. iOS only lets the microphone start while the app is in front.

    /// Idempotent, because every path into the app calls it.
    func arm() async {
        guard !micIsOn else { return }
        guard await AVAudioApplication.requestRecordPermission() else {
            state.problem = "Microphone access is off."
            publish()
            return
        }
        do {
            try microphone.start()
        } catch {
            state.problem = "The microphone would not start."
            publish()
            return
        }
        micIsOn = true
        state.problem = nil
        if state.phase == .off { state.phase = .armed }
        publish()
        scheduleDisarm()
    }

    /// Drops a take still being recorded. One already being written finishes, so
    /// its words are not lost.
    func turnOff() {
        disarmAt?.cancel()
        if state.phase == .recording {
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
        stage = .waiting
        engine.restart(previews: true)
    }

    /// A keyboard tap brought the app forward. Arm, then go straight back, unless
    /// the model still has to download or the tap came from this app's own field.
    func openedFromKeyboard() {
        let wasInFront = UIApplication.shared.applicationState == .active && hasBeenInFront
        Task {
            await arm()
            guard micIsOn, Engine.isModelPrepared else { return }
            if wasInFront {
                Bridge.remove("handoff.json")
                startTake()
                return
            }
            let host = Bridge.read(Handoff.self, "handoff.json")?.host
            if let host, HostReturn.open(bundle: host) { return }
            showSwipeBack = true
        }
    }

    private func scheduleDisarm() {
        disarmAt?.cancel()
        guard state.phase == .armed, let seconds = readyFor.seconds else { return }
        let work = DispatchWorkItem { [weak self] in self?.turnOff() }
        disarmAt = work
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds, execute: work)
    }

    // MARK: Takes

    private func startTake() {
        guard state.phase == .armed else { return }
        if case .failed(let message) = stage {
            state.problem = message
            publish()
            return
        }
        disarmAt?.cancel()
        Bridge.remove("words.json")
        // iOS can reclaim the memory-mapped weights while the app sits idle. As on
        // the Mac, page them back in during the take rather than after it.
        engine.warm()
        engine.begin()
        let engine = self.engine
        microphone.beginCapture { engine.feed($0) }
        state.phase = .recording
        state.problem = nil
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

    private func stopTake() {
        guard state.phase == .recording else { return }
        stopWatchingKeyboard()
        state.phase = .transcribing
        publish()
        takeTask = UIApplication.shared.beginBackgroundTask { [weak self] in
            MainActor.assumeIsolated { self?.endTakeTask() }
        }
        // The tap delivers audio in blocks, so the last word is still in one.
        let work = DispatchWorkItem { [self] in
            pendingFinish = nil
            microphone.endCapture()
            job = engine.finish()
            if job == 0 { settle(problem: "The speech engine stopped.") }
        }
        pendingFinish = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: work)
    }

    private func cancelTake() {
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
        guard finishedJob == job else { return }
        job = 0
        switch result {
        case .success(let text):
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty {
                settle(problem: nil)
                return
            }
            let now = Date()
            state.take = Take(id: Int64(now.timeIntervalSince1970 * 1000), text: trimmed, finished: now)
            History.append(trimmed, at: now)
            recent = History.recent(limit: 30)
            settle(problem: nil)
        case .failure(let failure):
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
                guard let self, Date().timeIntervalSince(self.lastPing) > Self.keyboardGoneAfter else { return }
                self.stopTake()
            }
        }
    }

    private func stopWatchingKeyboard() {
        keyboardWatch?.invalidate()
        keyboardWatch = nil
    }

    // MARK: The system taking the microphone away, or the engine failing

    /// Says nothing, because the next tap brings the mic back.
    private func micLost() {
        guard micIsOn else { return }
        if state.phase == .recording { stopTake() }
        turnOff()
    }

    private func routeChanged() {
        guard micIsOn else { return }
        do {
            try microphone.restart()
        } catch {
            micLost()
        }
    }

    private func stageChanged(_ newStage: Engine.Stage) {
        stage = newStage
        guard case .failed(let message) = newStage else { return }
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
