import UIKit

/// Shows what the app reports and sends it signals. All the microphone and engine
/// work happens in the app, because iOS gives keyboards no microphone.
final class KeyboardViewController: UIInputViewController {
    private var state = SessionState()
    private var lastPong = Date.distantPast
    private var waiting: [UUID: (Bool) -> Void] = [:]
    private var heartbeat: Timer?
    private var pressStartedTake = false
    private var pressedAt = Date()
    /// `insertText` is silently dropped while the keyboard is off screen.
    private var isActive = false
    private var hasLeft = false
    /// A message of the keyboard's own, which the heartbeat must not repaint over.
    private var note: String?

    private let bar = StartBar()
    private let page = LivePage()
    private let cancel = CloseButton()
    private let keys = UIStackView()
    private let globe = Key(symbol: "globe", label: "Next keyboard")
    /// Covers the faint keys during a take, so a tap there finishes it too.
    private let finishArea = UIControl()
    private var deleteRepeat: Timer?
    private var activatedAt = Date.distantPast
    /// False once the field has failed to hold the words, for the rest of the take.
    private var wordsInField = true

    /// For the label only. A slow answer must not read as the app having died,
    /// so this is looser than the check a tap makes.
    private var looksAlive: Bool { Date().timeIntervalSince(lastPong) < 5 }
    private static let fieldSettles: TimeInterval = 0.6

    override func viewDidLoad() {
        super.viewDidLoad()
        layout()
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        hasLeft = false
        activate()
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        activate()
    }

    /// Coming back from the app, iOS can build a new controller and show it
    /// without calling either appear method, so having a window is the sign to trust.
    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        if view.window != nil, !hasLeft { activate() }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        hasLeft = true
        isActive = false
        Diagnostics.log("keyboard_hidden", ["phase": state.phase.rawValue])
        // Leaving must end a take. Otherwise a tapped one records what comes next.
        if state.phase == .recording {
            Diagnostics.log("stop_sent", ["by": "leaving"])
            Signals.post(.stop)
        }
        heartbeat?.invalidate()
        heartbeat = nil
        deleteUp()
        bar.stop()
        page.stop()
    }

    private func activate() {
        guard !isActive else { return }
        isActive = true
        activatedAt = Date()
        // Registered here, because the signals go to whichever controller registered last.
        Signals.observe(.changed) { [weak self] in self?.reload() }
        Signals.observe(.words) { [weak self] in self?.showWords() }
        Signals.observe(.pong) { [weak self] in
            guard let self else { return }
            self.lastPong = Date()
            let answered = self.waiting.values
            self.waiting.removeAll()
            answered.forEach { $0(true) }
            self.render()
        }
        HostCapture.refresh()
        reload()
        Diagnostics.log("keyboard_shown", ["phase": state.phase.rawValue, "full_access": hasFullAccess])
        Signals.post(.ping)
        let beat = Timer(timeInterval: 1, repeats: true) { [weak self] _ in
            Signals.post(.ping)
            self?.render()
        }
        RunLoop.main.add(beat, forMode: .common)
        heartbeat = beat
        switchAwayFromFieldsThatNeedTheirOwnKeys()
        startAfterReturningFromTheApp()
    }

    override func textWillChange(_ textInput: UITextInput?) {}

    override func textDidChange(_ textInput: UITextInput?) {
        switchAwayFromFieldsThatNeedTheirOwnKeys()
    }

    private func switchAwayFromFieldsThatNeedTheirOwnKeys() {
        switch textDocumentProxy.keyboardType {
        case .numberPad, .phonePad, .decimalPad, .asciiCapableNumberPad, .numbersAndPunctuation, .emailAddress:
            Diagnostics.log("switched_away", ["keyboard_type": textDocumentProxy.keyboardType?.rawValue ?? -1])
            advanceToNextInputMode()
        default:
            break
        }
    }

    // MARK: Following the app

    private func reload() {
        let read = Bridge.read(SessionState.self, "state.json")
        if read == nil { Diagnostics.log("state_unreadable") }
        let previous = state.phase
        state = read ?? SessionState()
        if state.phase != previous {
            Diagnostics.log(
                "phase_seen",
                ["from": previous.rawValue, "to": state.phase.rawValue, "problem": state.problem ?? "none"])
        }
        settleField()
        showWords()
        render()
    }

    /// Calls made right after coming back can be lost, with the field left as it was.
    private func settleField() {
        let wait = Self.fieldSettles - Date().timeIntervalSince(activatedAt)
        guard wait <= 0 else {
            DispatchQueue.main.asyncAfter(deadline: .now() + wait) { [weak self] in self?.settleField() }
            return
        }
        insertFinishedTake()
        // A cancelled or failed take has no words to replace these with.
        if state.phase != .recording, state.phase != .transcribing { _ = takeDraftBack() }
    }

    /// The words go in the field, where the take will land, as marked text: what
    /// a keyboard for Japanese uses for text it has not committed yet. A field that
    /// ignores it, or a cursor that moved, sends them to the keyboard instead.
    private func showWords() {
        guard state.phase == .recording || state.phase == .transcribing else {
            wordsInField = true
            page.show(words: "")
            return
        }
        let words = Bridge.read(String.self, "words.json") ?? ""
        if wordsInField, state.phase == .recording, isActive, !words.isEmpty, !putInField(words) {
            Diagnostics.log("words_left_field", ["chars": words.count])
            wordsInField = false
            render()
        }
        page.show(words: wordsInField ? "" : words)
    }

    private func putInField(_ words: String) -> Bool {
        let proxy = textDocumentProxy
        let before = proxy.documentContextBeforeInput
        let last = Bridge.read(Draft.self, "draft.json")
        if let last {
            guard last.words != words else { return true }
            // iOS leaves marked text out of the context, so this stays as it was
            // unless the draft got committed or the cursor went elsewhere.
            guard before == last.before, proxy.hasText else {
                _ = takeDraftBack()
                return false
            }
        }
        let draft = Draft(before: before, spacing: last?.spacing ?? Spacing.prefix(before: before), words: words)
        Bridge.write(draft, "draft.json")
        proxy.setMarkedText(draft.text, selectedRange: NSRange(location: (draft.text as NSString).length, length: 0))
        return true
    }

    private func takeDraftBack() -> Draft? {
        guard isActive, let draft = Bridge.read(Draft.self, "draft.json") else { return nil }
        Bridge.remove("draft.json")
        let proxy = textDocumentProxy
        let before = proxy.documentContextBeforeInput
        if before != draft.before, let before, Self.endsWith(before, draft.text) {
            // Marked text is never in the context, so iOS committed this one when
            // the keyboard went away, and it would stay behind the real take.
            Diagnostics.log("draft_cleared", ["how": "deleted", "chars": draft.text.count])
            for _ in draft.text { proxy.deleteBackward() }
            return draft
        }
        Diagnostics.log("draft_cleared", ["how": "unmarked", "chars": draft.text.count])
        // A controller that did not set the mark cannot clear it with an empty one.
        proxy.setMarkedText(" ", selectedRange: NSRange(location: 1, length: 0))
        proxy.setMarkedText("", selectedRange: NSRange(location: 0, length: 0))
        proxy.unmarkText()
        return draft
    }

    /// The context can be cut to the last few words, so this compares only the end.
    private static func endsWith(_ context: String, _ text: String) -> Bool {
        let length = min(context.count, text.count, 12)
        return length > 0 && context.suffix(length) == text.suffix(length)
    }

    /// Once per take, because the bookmark survives this process being replaced.
    private func insertFinishedTake() {
        guard isActive, let take = state.take else { return }
        let inserted = Bridge.read(Int64.self, "inserted.json") ?? 0
        guard take.id > inserted else { return }
        Bridge.write(take.id, "inserted.json")
        let age = Date().timeIntervalSince(take.finished)
        guard age < Bridge.insertWindow else {
            Diagnostics.log("insert_stale", ["age_s": age, "chars": take.text.count])
            return
        }
        let draft = takeDraftBack()
        let spacing = draft?.spacing ?? Spacing.prefix(before: textDocumentProxy.documentContextBeforeInput)
        Diagnostics.log("inserted", ["chars": take.text.count, "age_s": age, "draft": draft != nil])
        textDocumentProxy.insertText(spacing + take.text)
    }

    private func startAfterReturningFromTheApp() {
        guard let handoff = Bridge.read(Handoff.self, "handoff.json") else { return }
        Bridge.remove("handoff.json")
        let age = Date().timeIntervalSince(handoff.asked)
        guard age < Bridge.handoffWindow else {
            Diagnostics.log("handoff_expired", ["age_s": age])
            return
        }
        whenAlive { [weak self] alive in
            guard alive, self?.state.phase == .armed else {
                Diagnostics.log(
                    "handoff_start_failed", ["alive": alive, "phase": self?.state.phase.rawValue ?? "none"])
                return
            }
            Diagnostics.log("start_sent", ["by": "handoff", "age_s": age])
            Signals.post(.start)
        }
    }

    /// Answers from the last heartbeat when it can. Otherwise it asks and waits.
    private func whenAlive(_ then: @escaping (Bool) -> Void) {
        if Date().timeIntervalSince(lastPong) < 2 {
            then(true)
            return
        }
        let id = UUID()
        waiting[id] = then
        Signals.post(.ping)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
            guard let then = self?.waiting.removeValue(forKey: id) else { return }
            Diagnostics.log("ping_timeout")
            then(false)
        }
    }

    // MARK: The mic button. A short tap toggles. A hold stops on release.

    @objc private func micDown() {
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        pressedAt = Date()
        pressStartedTake = false
        note = nil
        Diagnostics.log(
            "tap", ["phase": state.phase.rawValue, "pong_age_s": min(Date().timeIntervalSince(lastPong), 9999)])
        guard hasFullAccess else {
            Diagnostics.log("tap_no_full_access")
            openSettings()
            return
        }
        // Asking first would only delay the app opening when the mic is known to be off.
        guard state.phase != .off else {
            Diagnostics.log("tap_app_off")
            openApp()
            return
        }
        whenAlive { [weak self] alive in
            guard let self else { return }
            guard alive else {
                Diagnostics.log("tap_app_dead", ["phase": self.state.phase.rawValue])
                self.openApp()
                return
            }
            if self.state.phase == .armed {
                Diagnostics.log("start_sent", ["by": "tap"])
                self.pressStartedTake = true
                Signals.post(.start)
            } else if self.state.phase != .recording {
                Diagnostics.log("tap_not_armed", ["phase": self.state.phase.rawValue])
            }
        }
    }

    @objc private func micUp() {
        let held = Date().timeIntervalSince(pressedAt) > 0.35
        if pressStartedTake && !held { return }
        if state.phase == .recording || pressStartedTake {
            Diagnostics.log("stop_sent", ["by": held ? "release" : "tap", "phase": state.phase.rawValue])
            Signals.post(.stop)
        }
    }

    @objc private func cancelTapped() {
        Diagnostics.log("cancel_sent", ["phase": state.phase.rawValue])
        Signals.post(.cancel)
    }

    private func openApp() {
        Bridge.write(Handoff(asked: Date(), host: HostCapture.host), "handoff.json")
        Diagnostics.log("open_app", ["host": HostCapture.host != nil])
        // A keyboard has no public way to open a URL. The app's object sits on its
        // responder chain and accepts the call.
        var responder: UIResponder? = self
        while let current = responder {
            if let application = current as? UIApplication {
                application.open(Bridge.armURL, options: [:]) { [weak self] opened in
                    guard !opened else { return }
                    Diagnostics.log("open_app_refused")
                    self?.note = "Open see.computer to turn on the mic"
                    self?.render()
                }
                return
            }
            responder = current.next
        }
        Diagnostics.log("open_app_unreachable")
        note = "Open see.computer to turn on the mic"
        render()
    }

    @objc private func openSettings() {
        var responder: UIResponder? = self
        while let current = responder {
            if let application = current as? UIApplication {
                application.open(URL(string: "app-settings:")!, options: [:], completionHandler: nil)
                return
            }
            responder = current.next
        }
    }

    // MARK: Typing

    @objc private func typed(_ key: Key) {
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        let straight = key.output == "\u{2019}" && textDocumentProxy.smartQuotesType == .no
        textDocumentProxy.insertText(straight ? "'" : key.output)
    }

    @objc private func deleteDown() {
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        textDocumentProxy.deleteBackward()
        deleteRepeat?.invalidate()
        deleteRepeat = Timer.scheduledTimer(withTimeInterval: 0.45, repeats: false) { [weak self] _ in
            self?.deleteRepeat = Timer.scheduledTimer(withTimeInterval: 0.09, repeats: true) { _ in
                self?.textDocumentProxy.deleteBackward()
            }
        }
    }

    @objc private func deleteUp() {
        deleteRepeat?.invalidate()
        deleteRepeat = nil
    }

    // MARK: Drawing

    private func render() {
        let phase: SessionState.Phase = looksAlive ? state.phase : .off
        let talking = phase == .recording || phase == .transcribing
        let message = hasFullAccess ? note ?? state.problem : "Allow Full Access"
        bar.show(phase, message: message, since: state.since)
        cancel.isHidden = phase != .recording
        finishArea.isHidden = !talking
        let wordsHere = talking && !wordsInField
        page.isHidden = !wordsHere
        keys.isHidden = wordsHere
        keys.alpha = talking ? 0.35 : 1
        globe.isHidden = !needsInputModeSwitchKey
        page.show(phase)
    }

    private func layout() {
        guard let root = view else { return }
        let height = root.heightAnchor.constraint(equalToConstant: 262)
        height.priority = .defaultHigh
        height.isActive = true

        for control in [bar, finishArea] as [UIControl] {
            control.addTarget(self, action: #selector(micDown), for: .touchDown)
            control.addTarget(self, action: #selector(micUp), for: [.touchUpInside, .touchUpOutside, .touchCancel])
        }
        cancel.addTarget(self, action: #selector(cancelTapped), for: .touchUpInside)
        globe.addTarget(self, action: #selector(handleInputModeList(from:with:)), for: .allTouchEvents)

        let marks = [".", ",", "?", "!", "\u{2019}"].map { Key(title: $0) }
        let delete = Key(symbol: "delete.left", label: "Delete")
        let space = Key(label: "Space", output: " ")
        let enter = Key(symbol: "return.left", label: "Return", output: "\n")
        for key in marks + [space, enter] { key.addTarget(self, action: #selector(typed(_:)), for: .touchUpInside) }
        delete.addTarget(self, action: #selector(deleteDown), for: .touchDown)
        delete.addTarget(self, action: #selector(deleteUp), for: [.touchUpInside, .touchUpOutside, .touchCancel])

        let upper = UIStackView(arrangedSubviews: marks + [delete])
        upper.distribution = .fillEqually
        let lower = UIStackView(arrangedSubviews: [globe, space, enter])
        for row in [upper, lower] {
            row.spacing = 6
            row.heightAnchor.constraint(equalToConstant: 46).isActive = true
            keys.addArrangedSubview(row)
        }
        keys.axis = .vertical
        keys.spacing = 8

        // The finish area lies over the faint keys, so a tap there ends the take too.
        for child in [bar, keys, finishArea, page, cancel] as [UIView] {
            child.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(child)
        }
        NSLayoutConstraint.activate([
            bar.topAnchor.constraint(equalTo: root.topAnchor, constant: 8),
            bar.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 8),
            bar.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -8),

            keys.topAnchor.constraint(equalTo: bar.bottomAnchor, constant: 8),
            keys.leadingAnchor.constraint(equalTo: bar.leadingAnchor),
            keys.trailingAnchor.constraint(equalTo: bar.trailingAnchor),
            keys.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -8),
            globe.widthAnchor.constraint(equalToConstant: 46),
            enter.widthAnchor.constraint(equalToConstant: 116),

            finishArea.topAnchor.constraint(equalTo: keys.topAnchor),
            finishArea.leadingAnchor.constraint(equalTo: keys.leadingAnchor),
            finishArea.trailingAnchor.constraint(equalTo: keys.trailingAnchor),
            finishArea.bottomAnchor.constraint(equalTo: keys.bottomAnchor),

            page.topAnchor.constraint(equalTo: keys.topAnchor),
            page.leadingAnchor.constraint(equalTo: keys.leadingAnchor),
            page.trailingAnchor.constraint(equalTo: keys.trailingAnchor),
            page.bottomAnchor.constraint(equalTo: keys.bottomAnchor),

            cancel.topAnchor.constraint(equalTo: bar.topAnchor, constant: 3),
            cancel.leadingAnchor.constraint(equalTo: bar.leadingAnchor, constant: 3),
            cancel.widthAnchor.constraint(equalToConstant: 44),
            cancel.heightAnchor.constraint(equalToConstant: 44),
        ])
    }
}

/// What goes between the text already in the field and the new take.
enum Spacing {
    static func prefix(before: String?) -> String {
        guard let last = before?.last else { return "" }
        return last.isWhitespace || "([{\"'/-\u{201C}\u{2018}".contains(last) ? "" : " "
    }
}

/// The system keyboard's own key colours, so the bar and the keys read as one keyboard.
private enum Ink {
    static let key = UIColor {
        $0.userInterfaceStyle == .dark ? UIColor(red: 0.345, green: 0.345, blue: 0.357, alpha: 1) : .white
    }
    static let keyDown = UIColor {
        $0.userInterfaceStyle == .dark ? UIColor(white: 0.24, alpha: 1) : UIColor(red: 0.7, green: 0.72, blue: 0.76, alpha: 1)
    }
    static let edge = UIColor { UIColor(white: 0, alpha: $0.userInterfaceStyle == .dark ? 0.4 : 0.16) }
    static let mark = UIColor { $0.userInterfaceStyle == .dark ? .white : UIColor(white: 0.11, alpha: 1) }
    static let markDim = mark.withAlphaComponent(0.6)
}

/// A rounded face with the one-point lower edge that system keys have.
private func keyFace(radius: CGFloat, in control: UIControl) -> UIView {
    let edge = UIView()
    let face = UIView()
    for (view, color) in [(edge, Ink.edge), (face, Ink.key)] {
        view.backgroundColor = color
        view.layer.cornerRadius = radius
        view.layer.cornerCurve = .continuous
        view.isUserInteractionEnabled = false
        view.translatesAutoresizingMaskIntoConstraints = false
        control.addSubview(view)
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: control.leadingAnchor),
            view.trailingAnchor.constraint(equalTo: control.trailingAnchor),
            view.topAnchor.constraint(equalTo: control.topAnchor, constant: view === edge ? 1 : 0),
            view.bottomAnchor.constraint(equalTo: control.bottomAnchor, constant: view === edge ? 1 : 0),
        ])
    }
    return face
}

/// Start, then Done. It keeps its place and colour through a take, so what
/// changes is what it says, the clock in its corner, and the keys going faint.
final class StartBar: UIControl {
    private var face: UIView!
    private let middle = UIStackView()
    private let glyph = UIImageView()
    private let label = UILabel()
    private let spinner = UIActivityIndicatorView(style: .medium)
    private let corner = UIStackView()
    private let clock = UILabel()
    private let meter = Meter()

    override init(frame: CGRect) {
        super.init(frame: frame)
        face = keyFace(radius: 16, in: self)
        glyph.tintColor = Ink.mark
        glyph.contentMode = .scaleAspectFit
        label.textColor = Ink.mark
        label.numberOfLines = 2
        label.textAlignment = .center
        clock.textColor = Ink.mark
        clock.font = .monospacedDigitSystemFont(ofSize: 15, weight: .semibold)
        spinner.color = Ink.mark
        spinner.hidesWhenStopped = true
        middle.axis = .horizontal
        middle.alignment = .center
        middle.spacing = 8
        [glyph, label].forEach(middle.addArrangedSubview)
        corner.axis = .horizontal
        corner.alignment = .center
        corner.spacing = 8
        [clock, meter].forEach(corner.addArrangedSubview)
        for child in [middle, spinner, corner] as [UIView] {
            child.translatesAutoresizingMaskIntoConstraints = false
            child.isUserInteractionEnabled = false
            addSubview(child)
        }
        NSLayoutConstraint.activate([
            middle.centerXAnchor.constraint(equalTo: centerXAnchor),
            middle.centerYAnchor.constraint(equalTo: centerYAnchor),
            middle.widthAnchor.constraint(lessThanOrEqualTo: widthAnchor, constant: -32),
            spinner.centerXAnchor.constraint(equalTo: centerXAnchor),
            spinner.centerYAnchor.constraint(equalTo: centerYAnchor),
            corner.topAnchor.constraint(equalTo: topAnchor, constant: 12),
            corner.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
            corner.heightAnchor.constraint(equalToConstant: 28),
        ])
        isAccessibilityElement = true
        accessibilityTraits = .button
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    /// `message` is for what a tap alone will not fix. Everything else goes unsaid.
    func show(_ phase: SessionState.Phase, message: String?, since: Date?) {
        middle.isHidden = phase == .transcribing
        corner.isHidden = phase != .recording
        if phase == .transcribing { spinner.startAnimating() } else { spinner.stopAnimating() }
        var font = UIFont.systemFont(ofSize: 20, weight: .semibold)
        switch phase {
        case .off, .armed:
            glyph.image = UIImage(systemName: message == nil ? "mic.fill" : "mic.slash")
            label.text = message ?? "Start"
            if message != nil { font = .systemFont(ofSize: 15, weight: .medium) }
            accessibilityLabel = message ?? "Start dictation"
        case .recording:
            glyph.image = UIImage(systemName: "checkmark")
            label.text = "Done"
            accessibilityLabel = "Finish"
        case .transcribing:
            accessibilityLabel = "Writing"
        }
        label.font = font
        glyph.preferredSymbolConfiguration = .init(pointSize: 20, weight: .semibold)
        if phase == .recording {
            let seconds = max(0, Int(Date().timeIntervalSince(since ?? Date())))
            clock.text = String(format: "%d:%02d", seconds / 60, seconds % 60)
            meter.start()
        } else {
            meter.stop()
        }
    }

    func stop() {
        meter.stop()
    }

    override var isHighlighted: Bool {
        didSet { face.backgroundColor = isHighlighted ? Ink.keyDown : Ink.key }
    }
}

/// A punctuation, space, return, delete or globe key.
final class Key: UIControl {
    let output: String
    private var face: UIView!

    init(title: String? = nil, symbol: String? = nil, label: String? = nil, output: String? = nil) {
        self.output = output ?? title ?? ""
        super.init(frame: .zero)
        face = keyFace(radius: 10, in: self)
        let content: UIView
        if let symbol {
            let image = UIImageView(image: UIImage(systemName: symbol))
            image.tintColor = Ink.mark
            image.preferredSymbolConfiguration = .init(pointSize: 19, weight: .regular)
            content = image
        } else {
            let text = UILabel()
            text.text = title
            text.textColor = Ink.mark
            text.font = .systemFont(ofSize: 24)
            content = text
        }
        content.translatesAutoresizingMaskIntoConstraints = false
        content.isUserInteractionEnabled = false
        addSubview(content)
        content.centerXAnchor.constraint(equalTo: centerXAnchor).isActive = true
        content.centerYAnchor.constraint(equalTo: centerYAnchor).isActive = true
        isAccessibilityElement = true
        accessibilityTraits = .keyboardKey
        accessibilityLabel = label ?? title
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override var isHighlighted: Bool {
        didSet { face.backgroundColor = isHighlighted ? Ink.keyDown : Ink.key }
    }

    // Half the gap on each side, so a tap between two keys still lands on one.
    override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
        bounds.insetBy(dx: -3, dy: -4).contains(point)
    }
}

/// How loud the microphone hears you, from the level the app writes.
final class Meter: UIStackView {
    private var heights: [NSLayoutConstraint] = []
    private var levels = [Float](repeating: 0, count: 7)
    private var link: CADisplayLink?
    private var frames = 0

    override init(frame: CGRect) {
        super.init(frame: frame)
        axis = .horizontal
        alignment = .center
        spacing = 3
        for _ in levels {
            let bar = UIView()
            bar.layer.cornerRadius = 1.5
            bar.backgroundColor = Ink.mark.withAlphaComponent(0.75)
            bar.translatesAutoresizingMaskIntoConstraints = false
            bar.widthAnchor.constraint(equalToConstant: 3).isActive = true
            let height = bar.heightAnchor.constraint(equalToConstant: 3)
            height.isActive = true
            heights.append(height)
            addArrangedSubview(bar)
        }
        heightAnchor.constraint(equalToConstant: 24).isActive = true
    }

    required init(coder: NSCoder) { fatalError("not used") }

    func start() {
        guard link == nil else { return }
        let link = CADisplayLink(target: self, selector: #selector(tick))
        link.preferredFrameRateRange = CAFrameRateRange(minimum: 15, maximum: 30, preferred: 30)
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    func stop() {
        link?.invalidate()
        link = nil
        levels = [Float](repeating: 0, count: levels.count)
        heights.forEach { $0.constant = 3 }
    }

    @objc private func tick() {
        frames += 1
        guard frames % 2 == 0 else { return }
        levels.removeFirst()
        levels.append(Bridge.readLevel())
        for (index, height) in heights.enumerated() {
            height.constant = 3 + CGFloat(levels[index]) * 19
        }
    }
}

/// The words so far, over the faint keys, when the field cannot hold them. It
/// takes no touches, so they reach the finish area below.
final class LivePage: UIView {
    private let clip = UIView()
    private let words = UILabel()
    private let fade = CAGradientLayer()
    private var link: CADisplayLink?
    private var shown = ""
    /// UTF-16 length of the part that has not changed since the last preview.
    private var settled = 0
    private var fadeStarted: CFTimeInterval?
    private var writing = false

    private static let fadeTime: CFTimeInterval = 0.25

    override init(frame: CGRect) {
        super.init(frame: frame)
        isUserInteractionEnabled = false
        clip.clipsToBounds = true
        fade.colors = [UIColor.clear.cgColor, UIColor.black.cgColor]
        fade.locations = [0, 1]
        clip.layer.mask = fade
        words.numberOfLines = 0
        clip.translatesAutoresizingMaskIntoConstraints = false
        words.translatesAutoresizingMaskIntoConstraints = false
        addSubview(clip)
        clip.addSubview(words)
        NSLayoutConstraint.activate([
            clip.topAnchor.constraint(equalTo: topAnchor),
            clip.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            clip.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            clip.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -4),
            words.leadingAnchor.constraint(equalTo: clip.leadingAnchor),
            words.trailingAnchor.constraint(equalTo: clip.trailingAnchor),
            words.bottomAnchor.constraint(equalTo: clip.bottomAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override func layoutSubviews() {
        super.layoutSubviews()
        fade.frame = clip.bounds
        fade.endPoint = CGPoint(x: 0.5, y: min(1, 10 / max(clip.bounds.height, 1)))
        fade.startPoint = CGPoint(x: 0.5, y: 0)
    }

    func show(_ phase: SessionState.Phase) {
        writing = phase == .transcribing
        if phase == .recording || writing { start() } else { stop() }
        paint()
    }

    func show(words new: String) {
        guard new != shown else { return }
        settled = Self.settledLength(from: shown, to: new)
        shown = new
        fadeStarted = CACurrentMediaTime()
        paint()
    }

    /// The whole words that stayed the same, so only new or rewritten ones fade in.
    private static func settledLength(from old: String, to new: String) -> Int {
        var same = old.commonPrefix(with: new)
        let wholeWord = same == old && new.dropFirst(same.count).first == " "
        if !wholeWord {
            guard let space = same.lastIndex(of: " ") else { return 0 }
            same = String(same[..<space])
        }
        return (same as NSString).length
    }

    private func paint() {
        let font = UIFont.systemFont(ofSize: 17)
        let ink = Ink.mark
        let base = ink.withAlphaComponent(writing ? 0.5 : 1)
        let text = NSMutableAttributedString(string: shown, attributes: [.font: font, .foregroundColor: base])
        if let started = fadeStarted {
            let progress = (CACurrentMediaTime() - started) / Self.fadeTime
            if progress >= 1 {
                fadeStarted = nil
            } else {
                let range = NSRange(location: settled, length: (shown as NSString).length - settled)
                text.addAttribute(.foregroundColor, value: ink.withAlphaComponent(0.2 + 0.8 * progress), range: range)
            }
        }
        words.attributedText = text
    }

    private func start() {
        guard link == nil else { return }
        let link = CADisplayLink(target: self, selector: #selector(tick))
        link.preferredFrameRateRange = CAFrameRateRange(minimum: 15, maximum: 30, preferred: 30)
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    func stop() {
        link?.invalidate()
        link = nil
    }

    @objc private func tick() {
        if fadeStarted != nil { paint() }
    }
}

final class CloseButton: UIControl {
    private let glyph = UIImageView(image: UIImage(systemName: "xmark"))

    override init(frame: CGRect) {
        super.init(frame: frame)
        glyph.tintColor = Ink.markDim
        glyph.preferredSymbolConfiguration = .init(pointSize: 15, weight: .bold)
        glyph.translatesAutoresizingMaskIntoConstraints = false
        glyph.isUserInteractionEnabled = false
        addSubview(glyph)
        glyph.centerXAnchor.constraint(equalTo: centerXAnchor).isActive = true
        glyph.centerYAnchor.constraint(equalTo: centerYAnchor).isActive = true
        accessibilityLabel = "Cancel"
        isAccessibilityElement = true
        accessibilityTraits = .button
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override var isHighlighted: Bool {
        didSet { glyph.alpha = isHighlighted ? 0.5 : 1 }
    }

    // A cancel target this small needs some room around it.
    override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
        bounds.insetBy(dx: -6, dy: -6).contains(point)
    }
}
