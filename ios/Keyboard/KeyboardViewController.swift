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
    private let completions = CompletionStrip()
    private let status = UILabel()
    private let meter = Meter()
    private let statusRow = UIStackView()
    private var liveWords = ""
    /// Saved across keyboard-controller replacement. A partial transcript that
    /// was already edited must never be overwritten by the final transcript.
    private struct ManualEdit: Codable {
        let since: Date
        let field: UUID
        let previewStillInField: Bool
        var hadTextAfterEdit: Bool?
    }
    private var manualEdit: ManualEdit?
    private let cancel = CloseButton()
    private let keys = TypingKeys()
    /// Destructive edits require the same field, selection and surrounding text.
    private struct Caret: Equatable {
        let field: UUID
        let before: String
        let after: String?

        init?(_ proxy: UITextDocumentProxy) {
            guard proxy.selectedText?.isEmpty != false, let before = proxy.documentContextBeforeInput else { return nil }
            field = proxy.documentIdentifier
            self.before = before
            after = proxy.documentContextAfterInput
        }
    }
    private var lastSpace: (time: TimeInterval, caret: Caret)?
    private var lastLetter: Caret?
    private var suggestionSpace: Caret?
    private var lastCorrection: (caret: Caret, inserted: String, original: String)?
    private var deleteRepeat: Timer?
    private var pendingInsert = false
    private var activatedAt = Date.distantPast
    /// False once the field has failed to hold the words, for the rest of the take.
    private var wordsInField = true
    /// The field was sent mid-take, so hiding next is not the user walking away.
    private var takeWasSent = false
    private struct Selection: Equatable {
        let text: String
        let before: String?
        let after: String?
        let field: UUID

        init?(_ proxy: UITextDocumentProxy) {
            guard let text = proxy.selectedText, !text.isEmpty else { return nil }
            self.text = text
            before = proxy.documentContextBeforeInput
            after = proxy.documentContextAfterInput
            field = proxy.documentIdentifier
        }
    }
    private var selectionAtStart: Selection?

    /// For the label only. A slow answer must not read as the app having died,
    /// so this is looser than the check a tap makes.
    private var looksAlive: Bool { Date().timeIntervalSince(lastPong) < 5 }
    private static let fieldSettles: TimeInterval = 0.6

    override init(nibName nibNameOrNil: String?, bundle nibBundleOrNil: Bundle?) {
        super.init(nibName: nibNameOrNil, bundle: nibBundleOrNil)
        hasDictationKey = true
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        hasDictationKey = true
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        layout()
        requestSupplementaryLexicon { [weak self] lexicon in
            DispatchQueue.main.async { self?.completions.use(lexicon) }
        }
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        hasLeft = false
        activate()
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        hasDictationKey = true
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
        forgetTypingPosition()
        Diagnostics.log(
            "keyboard_hidden",
            [
                "phase": state.phase.rawValue, "has_text": textDocumentProxy.hasText,
                "draft": Bridge.read(Draft.self, "draft.json") != nil,
            ])
        // A take goes on while the user looks elsewhere. The Dynamic Island shows it
        // and can finish it. Sending the words is the one way of leaving that ends it.
        if !endTakeIfFieldWasSent(), state.phase == .recording, !takeWasSent {
            Diagnostics.log("away_sent")
            Signals.post(.away)
        }
        heartbeat?.invalidate()
        heartbeat = nil
        deleteUp()
        meter.stop()
    }

    private func activate() {
        guard !isActive else { return }
        isActive = true
        hasDictationKey = true
        keys.reset()
        configureTypingKeys()
        updateTypingContext()
        Signals.observe(.typingLanguagesChanged) { [weak self] in self?.configureTypingKeys() }
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

    private func configureTypingKeys() {
        let preference = TypingLanguages.current
        keys.configure(swedish: preference.usesSwedishKeys, needsGlobe: needsInputModeSwitchKey)
        completions.configure(preference)
        updateTypingContext()
    }

    private func updateTypingContext() {
        keys.updateCase(for: textDocumentProxy)
        completions.update(for: textDocumentProxy, canCorrect: lastLetter != nil && lastLetter == Caret(textDocumentProxy))
    }

    override func selectionDidChange(_ textInput: UITextInput?) {
        updateTypingContext()
    }

    override func textWillChange(_ textInput: UITextInput?) {}

    override func textDidChange(_ textInput: UITextInput?) {
        switchAwayFromFieldsThatNeedTheirOwnKeys()
        _ = endTakeIfFieldWasSent()
        updateTypingContext()
    }

    /// A chat app empties its field when it sends, live words included. Only the
    /// field can say so, because a keyboard never sees the send button.
    private func endTakeIfFieldWasSent() -> Bool {
        guard state.phase == .recording || state.phase == .transcribing, wordsInField, !takeWasSent,
            let draft = Bridge.read(Draft.self, "draft.json"), !textDocumentProxy.hasText
        else { return false }
        Diagnostics.log("sent_seen", ["phase": state.phase.rawValue, "chars": draft.text.count, "active": isActive])
        takeWasSent = true
        // The host can empty the field and still draw the words as marked. Once the
        // keyboard has gone it cannot clear them, so the draft stays for its return.
        if isActive {
            _ = takeDraftBack()
        } else {
            markDraftDropped()
        }
        Signals.post(.sent)
        return true
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
        let previousStart = state.since
        state = read ?? SessionState()
        if state.phase == .recording && state.since != previousStart { wordsInField = true }
        if let edit = Bridge.read(ManualEdit.self, "manual-edit.json"), edit.since == state.since {
            manualEdit = edit
            wordsInField = false
        } else {
            manualEdit = nil
        }
        if state.phase != previous {
            if state.phase == .recording { takeWasSent = false }
            Diagnostics.log(
                "phase_seen",
                ["from": previous.rawValue, "to": state.phase.rawValue, "problem": state.problem ?? "none"])
        }
        pendingInsert = state.take.map { $0.sent != true && $0.id > (Bridge.read(Int64.self, "inserted.json") ?? 0) } ?? false
        settleField()
        showWords()
        render()
    }

    /// Calls made right after coming back can be lost, with the field left as it was.
    private func settleField() {
        guard isActive else { return }
        let wait = Self.fieldSettles - Date().timeIntervalSince(activatedAt)
        guard wait <= 0 else {
            DispatchQueue.main.asyncAfter(deadline: .now() + wait) { [weak self] in self?.settleField() }
            return
        }
        insertFinishedTake()
        pendingInsert = false
        if state.phase != .recording, state.phase != .transcribing, manualEdit != nil {
            manualEdit = nil
            Bridge.remove("manual-edit.json")
        }
        // A take that ended with no transcript has no words to replace these with.
        if state.phase != .recording, state.phase != .transcribing,
            let draft = Bridge.read(Draft.self, "draft.json")
        {
            if draft.dropped == true { _ = takeDraftBack() } else { keepDraft(draft) }
        }
        showWords()
        updateTypingContext()
        render()
    }

    private var fieldHasSettled: Bool { Date().timeIntervalSince(activatedAt) >= Self.fieldSettles }

    /// The words go in the field, where the take will land, as marked text: what
    /// a keyboard for Japanese uses for text it has not committed yet. A field that
    /// ignores it, or a cursor that moved, sends them to the keyboard instead.
    private func showWords() {
        guard state.phase == .recording || state.phase == .transcribing else {
            wordsInField = true
            liveWords = ""
            return
        }
        let words = Bridge.read(String.self, "words.json") ?? ""
        if wordsInField, state.phase == .recording, !takeWasSent, isActive, fieldHasSettled, !words.isEmpty,
            !putInField(words)
        {
            Diagnostics.log("words_left_field", ["chars": words.count])
            wordsInField = false
            render()
        }
        liveWords = wordsInField ? "" : words
        if !wordsInField { render() }
    }

    private func putInField(_ words: String) -> Bool {
        let proxy = textDocumentProxy
        let before = proxy.documentContextBeforeInput
        guard proxy.selectedText?.isEmpty != false else { return false }
        let last = Bridge.read(Draft.self, "draft.json")
        if let last {
            guard last.words != words else { return true }
            if !Self.isMarked(last, before: before, in: proxy) {
                guard Self.isCommitted(last, before: before) else {
                    return false
                }
                // iOS committed the words when the keyboard went away mid-take.
                Diagnostics.log("draft_remarked", ["chars": last.text.count])
                for _ in last.text { proxy.deleteBackward() }
            }
        }
        let draft = Draft(
            before: last?.before ?? before, spacing: last?.spacing ?? Spacing.prefix(before: before), words: words,
            dropped: last?.dropped)
        Bridge.write(draft, "draft.json")
        proxy.setMarkedText(draft.text, selectedRange: NSRange(location: (draft.text as NSString).length, length: 0))
        return true
    }

    private func takeDraftBack() -> Draft? {
        guard isActive, let draft = Bridge.read(Draft.self, "draft.json") else { return nil }
        Bridge.remove("draft.json")
        let proxy = textDocumentProxy
        guard proxy.selectedText?.isEmpty != false else { return nil }
        if Self.isCommitted(draft, before: proxy.documentContextBeforeInput) {
            // It would stay behind the real take.
            Diagnostics.log("draft_cleared", ["how": "deleted", "chars": draft.text.count])
            for _ in draft.text { proxy.deleteBackward() }
            return draft
        }
        guard Self.isMarked(draft, before: proxy.documentContextBeforeInput, in: proxy) else { return nil }
        Diagnostics.log("draft_cleared", ["how": "unmarked", "chars": draft.text.count])
        // A controller that did not set the mark cannot clear it with an empty one.
        proxy.setMarkedText(" ", selectedRange: NSRange(location: 1, length: 0))
        proxy.setMarkedText("", selectedRange: NSRange(location: 0, length: 0))
        proxy.unmarkText()
        return draft
    }

    private func markDraftDropped() {
        guard var draft = Bridge.read(Draft.self, "draft.json") else { return }
        draft.dropped = true
        Bridge.write(draft, "draft.json")
    }

    /// The live words are the only copy of a take the app never finished, so they stay.
    private func keepDraft(_ draft: Draft) {
        guard isActive else { return }
        Bridge.remove("draft.json")
        let proxy = textDocumentProxy
        let before = proxy.documentContextBeforeInput
        if Self.isMarked(draft, before: before, in: proxy) {
            // A controller that did not set the mark has to set it before committing it.
            proxy.setMarkedText(draft.text, selectedRange: NSRange(location: (draft.text as NSString).length, length: 0))
            proxy.unmarkText()
            Diagnostics.log("draft_kept", ["how": "committed", "chars": draft.text.count])
        } else {
            Diagnostics.log(
                "draft_kept", ["how": Self.isCommitted(draft, before: before) ? "already" : "elsewhere", "chars": draft.text.count])
        }
    }

    /// iOS leaves marked text out of the context, so it stays as it was unless the
    /// draft got committed or the cursor went elsewhere.
    private static func isMarked(_ draft: Draft, before: String?, in proxy: UITextDocumentProxy) -> Bool {
        proxy.selectedText?.isEmpty != false && before == draft.before && proxy.hasText
    }

    /// Marked text is never in the context, so a context that ends in the draft
    /// means iOS committed it, which it does to UIKit fields when the keyboard goes away.
    private static func isCommitted(_ draft: Draft, before: String?) -> Bool {
        guard let before, before != draft.before else { return false }
        return endsWith(before, draft.text)
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
        guard take.sent != true else {
            Diagnostics.log("insert_skipped_sent", ["chars": take.text.count])
            return
        }
        if let edit = manualEdit, edit.since <= take.finished {
            Bridge.remove("manual-edit.json")
            manualEdit = nil
            guard !edit.previewStillInField, !(edit.hadTextAfterEdit == true && !textDocumentProxy.hasText),
                textDocumentProxy.documentIdentifier == edit.field,
                textDocumentProxy.selectedText?.isEmpty != false else { return }
        }
        let age = Date().timeIntervalSince(take.finished)
        let before = textDocumentProxy.documentContextBeforeInput
        let inItsField = Bridge.read(Draft.self, "draft.json").map {
            Self.isMarked($0, before: before, in: textDocumentProxy) || Self.isCommitted($0, before: before)
        } ?? false
        guard age < Bridge.insertWindow || (inItsField && age < Bridge.returnWindow) else {
            Diagnostics.log("insert_stale", ["age_s": age, "chars": take.text.count])
            return
        }
        // A selection made while the take was finishing belongs to the host.
        // A delayed result stays in History instead of replacing that selection.
        let selection = Selection(textDocumentProxy)
        guard selection == nil || selection == selectionAtStart else {
            Bridge.remove("draft.json")
            return
        }
        let draft = takeDraftBack()
        let spacing = draft?.spacing ?? Spacing.prefix(before: textDocumentProxy.documentContextBeforeInput)
        Diagnostics.log(
            "inserted", ["chars": take.text.count, "age_s": age, "draft": draft != nil, "in_its_field": inItsField])
        textDocumentProxy.insertText(selection == nil ? spacing + take.text : take.text)
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
            self?.startTake()
        }
    }

    private func startTake() {
        forgetTypingPosition()
        selectionAtStart = Selection(textDocumentProxy)
        Signals.post(.start)
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
                self.startTake()
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
        markDraftDropped()
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

    private func prepareForManualEdit() {
        guard state.phase == .recording || state.phase == .transcribing,
            manualEdit == nil, let since = state.since else { return }
        wordsInField = false
        let hadDraft = Bridge.read(Draft.self, "draft.json") != nil
        let removed = takeDraftBack() != nil
        let edit = ManualEdit(since: since, field: textDocumentProxy.documentIdentifier,
                              previewStillInField: hadDraft && !removed)
        manualEdit = edit
        Bridge.write(edit, "manual-edit.json")
        showWords()
    }

    private func rememberEditedField() {
        guard var edit = manualEdit, edit.hadTextAfterEdit != textDocumentProxy.hasText else { return }
        edit.hadTextAfterEdit = textDocumentProxy.hasText
        manualEdit = edit
        Bridge.write(edit, "manual-edit.json")
    }

    private func forgetTypingPosition() {
        lastSpace = nil
        lastLetter = nil
        lastCorrection = nil
        suggestionSpace = nil
    }

    private func replaceSuffix(_ original: String, with replacement: String) {
        let shared = zip(original, replacement).prefix { $0.0 == $0.1 }.count
        for _ in original.dropFirst(shared) { textDocumentProxy.deleteBackward() }
        let tail = String(replacement.dropFirst(shared))
        if !tail.isEmpty { textDocumentProxy.insertText(tail) }
    }

    private func chooseSuggestion(_ input: WordInput, _ candidate: String) {
        guard isActive, keys.isUserInteractionEnabled else { return }
        prepareForManualEdit()
        let proxy = textDocumentProxy
        guard WordInput(proxy) == input else { return }
        forgetTypingPosition()
        let next = proxy.documentContextAfterInput?.first
        let addSpace = next == nil || next.map { !$0.isWhitespace && !".,!?;:)]}".contains($0) } == true
        replaceSuffix(input.word, with: candidate + (addSpace ? " " : ""))
        if next == " " { proxy.adjustTextPosition(byCharacterOffset: 1) }
        if addSpace { suggestionSpace = Caret(proxy) }
        rememberEditedField()
        updateTypingContext()
    }

    private func typed(_ output: String) {
        guard isActive, keys.isUserInteractionEnabled else { return }
        prepareForManualEdit()
        let proxy = textDocumentProxy
        let caret = Caret(proxy)
        let previousSpace = lastSpace
        let followsLetter = caret != nil && caret == lastLetter
        let punctuation = output.count == 1 && ".,!?;:".contains(output)
        let removeSpace = punctuation && caret != nil && caret == suggestionSpace && caret?.before.last == " "
        forgetTypingPosition()
        let now = ProcessInfo.processInfo.systemUptime
        if output == " ", let previousSpace, now - previousSpace.time < 0.45, caret == previousSpace.caret {
            replaceSuffix(" ", with: ". ")
        } else {
            if removeSpace { proxy.deleteBackward() }
            if followsLetter, output == " " || punctuation,
               let input = WordInput(proxy), !input.word.isEmpty, let correction = completions.suggestions(for: input).automatic {
                let inserted = correction + output
                replaceSuffix(input.word, with: inserted)
                if let end = Caret(proxy), end.before.hasSuffix(inserted) {
                    lastCorrection = (end, inserted, input.word)
                }
            } else {
                proxy.insertText(output == "’" && proxy.smartQuotesType == .no ? "'" : output)
            }
            if output.count == 1, output.first?.isLetter == true {
                lastLetter = Caret(proxy)
            } else if output == " ", let prior = caret?.before.last, prior.isLetter || prior.isNumber,
                      let end = Caret(proxy), end.before.last == " " {
                lastSpace = (now, end)
            }
        }
        rememberEditedField()
        updateTypingContext()
    }

    private func deleteCharacter() {
        guard isActive, keys.isUserInteractionEnabled else { return }
        prepareForManualEdit()
        let correction = lastCorrection
        forgetTypingPosition()
        if let correction, correction.caret == Caret(textDocumentProxy) {
            replaceSuffix(correction.inserted, with: correction.original)
        } else {
            textDocumentProxy.deleteBackward()
        }
        rememberEditedField()
        updateTypingContext()
    }

    private func deleteDown() {
        guard isActive, keys.isUserInteractionEnabled else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        deleteCharacter()
        deleteRepeat?.invalidate()
        deleteRepeat = Timer.scheduledTimer(withTimeInterval: 0.45, repeats: false) { [weak self] _ in
            self?.deleteRepeat = Timer.scheduledTimer(withTimeInterval: 0.09, repeats: true) { [weak self] _ in
                self?.deleteCharacter()
            }
        }
    }

    @objc private func deleteUp() {
        deleteRepeat?.invalidate()
        deleteRepeat = nil
    }

    // MARK: Drawing

    private func render() {
        let phase: SessionState.Phase = looksAlive || !fieldHasSettled ? state.phase : .off
        let talking = phase == .recording || phase == .transcribing
        let message = hasFullAccess ? note ?? state.problem : "Allow Full Access"
        bar.show(phase)
        let preview = !wordsInField && !liveWords.isEmpty
        let elapsed = max(0, Int(Date().timeIntervalSince(state.since ?? Date())))
        status.text = talking ? (preview ? liveWords : String(format: "%d:%02d", elapsed / 60, elapsed % 60)) : message
        status.font = preview || !talking ? .systemFont(ofSize: 15) : .monospacedDigitSystemFont(ofSize: 15, weight: .medium)
        statusRow.isHidden = !talking && message == nil
        completions.isHidden = !statusRow.isHidden || textDocumentProxy.autocorrectionType == .no
        meter.isHidden = phase != .recording || preview
        if meter.isHidden { meter.stop() } else { meter.start() }
        cancel.isHidden = phase != .recording
        keys.isUserInteractionEnabled = !pendingInsert
        keys.accessibilityElementsHidden = pendingInsert
        if !keys.isUserInteractionEnabled { deleteUp() }
    }

    private func layout() {
        guard let root = view else { return }
        let height = root.heightAnchor.constraint(equalToConstant: 254)
        height.priority = .defaultHigh
        height.isActive = true
        root.clipsToBounds = true
        bar.addTarget(self, action: #selector(micDown), for: .touchDown)
        bar.addTarget(self, action: #selector(micUp), for: [.touchUpInside, .touchUpOutside, .touchCancel])
        statusRow.alignment = .center
        statusRow.spacing = 8
        status.lineBreakMode = .byTruncatingHead
        status.textColor = .secondaryLabel
        statusRow.addArrangedSubview(meter)
        statusRow.addArrangedSubview(status)
        cancel.addTarget(self, action: #selector(cancelTapped), for: .touchUpInside)
        keys.connectGlobe(to: self)
        keys.moveCursor = { [weak self] offset in
            self?.prepareForManualEdit()
            self?.forgetTypingPosition()
            self?.textDocumentProxy.adjustTextPosition(byCharacterOffset: offset)
        }
        completions.accept = { [weak self] input, candidate in self?.chooseSuggestion(input, candidate) }
        keys.insert = { [weak self] in self?.typed($0) }
        keys.startDelete = { [weak self] in self?.deleteDown() }
        keys.endDelete = { [weak self] in self?.deleteUp() }

        let controls = UIStackView(arrangedSubviews: [cancel, bar])
        controls.spacing = 4
        let cancelWidth = cancel.widthAnchor.constraint(equalToConstant: 44)
        cancelWidth.priority = .defaultHigh
        cancelWidth.isActive = true
        for child in [completions, statusRow, controls, keys] {
            child.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(child)
        }
        NSLayoutConstraint.activate([
            controls.topAnchor.constraint(equalTo: root.topAnchor, constant: 5),
            controls.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -12),
            controls.heightAnchor.constraint(equalToConstant: 44),

            bar.widthAnchor.constraint(equalToConstant: 108),
            completions.topAnchor.constraint(equalTo: controls.topAnchor),
            completions.bottomAnchor.constraint(equalTo: controls.bottomAnchor),
            completions.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 7),
            completions.trailingAnchor.constraint(equalTo: controls.leadingAnchor, constant: -6),
            statusRow.centerYAnchor.constraint(equalTo: controls.centerYAnchor),
            statusRow.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 14),
            statusRow.trailingAnchor.constraint(lessThanOrEqualTo: controls.leadingAnchor, constant: -6),
            keys.topAnchor.constraint(equalTo: controls.bottomAnchor),
            keys.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 4),
            keys.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -4),
            keys.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: 5)
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
        $0.userInterfaceStyle == .dark ? UIColor(white: 1, alpha: 0.165) : .white
    }
    static let keyDown = UIColor {
        $0.userInterfaceStyle == .dark ? UIColor(white: 0.24, alpha: 1) : UIColor(red: 0.7, green: 0.72, blue: 0.76, alpha: 1)
    }
    static let mark = UIColor { $0.userInterfaceStyle == .dark ? .white : UIColor(white: 0.11, alpha: 1) }
    static let markDim = mark.withAlphaComponent(0.6)
}

/// Flat keycap above the system keyboard material.
private func keyFace(radius: CGFloat, in control: UIControl, usesGlass: Bool = false, inset: CGSize = .zero) -> UIView {
    let face: UIView
    if #available(iOS 26, *), usesGlass {
        let glass = UIGlassEffect(style: .regular)
        glass.isInteractive = true
        let effectView = UIVisualEffectView(effect: glass)
        effectView.cornerConfiguration = .corners(radius: .fixed(radius))
        face = effectView
    } else {
        face = UIView()
        face.backgroundColor = Ink.key
    }
    face.layer.cornerRadius = radius
    face.layer.cornerCurve = .continuous
    face.isUserInteractionEnabled = false
    face.translatesAutoresizingMaskIntoConstraints = false
    control.addSubview(face)
    NSLayoutConstraint.activate([
        face.leadingAnchor.constraint(equalTo: control.leadingAnchor, constant: inset.width),
        face.trailingAnchor.constraint(equalTo: control.trailingAnchor, constant: -inset.width),
        face.topAnchor.constraint(equalTo: control.topAnchor, constant: inset.height),
        face.bottomAnchor.constraint(equalTo: control.bottomAnchor, constant: -inset.height)
    ])
    return face
}

/// The same target starts and finishes a take. Its glass is inset from both
/// the keyboard edge and the control's full touch area.
final class StartBar: UIControl {
    private let glyph = UIImageView()
    private let label = UILabel()

    override init(frame: CGRect) {
        super.init(frame: frame)
        let cap = UIControl()
        cap.isUserInteractionEnabled = false
        cap.translatesAutoresizingMaskIntoConstraints = false
        addSubview(cap)
        _ = keyFace(radius: 18, in: cap, usesGlass: true)
        let content = UIStackView(arrangedSubviews: [glyph, label])
        content.alignment = .center
        content.spacing = 6
        content.isUserInteractionEnabled = false
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        glyph.tintColor = Ink.mark
        glyph.preferredSymbolConfiguration = .init(pointSize: 16, weight: .semibold)
        label.textColor = Ink.mark
        label.font = .systemFont(ofSize: 16, weight: .semibold)
        NSLayoutConstraint.activate([
            cap.leadingAnchor.constraint(equalTo: leadingAnchor),
            cap.trailingAnchor.constraint(equalTo: trailingAnchor),
            cap.topAnchor.constraint(equalTo: topAnchor, constant: 3),
            cap.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -3),
            content.centerXAnchor.constraint(equalTo: centerXAnchor),
            content.centerYAnchor.constraint(equalTo: centerYAnchor)
        ])
        isAccessibilityElement = true
        accessibilityTraits = .button
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    func show(_ phase: SessionState.Phase) {
        switch phase {
        case .off, .armed:
            glyph.image = UIImage(systemName: "mic.fill")
            label.text = "Dictate"
            accessibilityLabel = "Start dictation"
        case .recording:
            glyph.image = UIImage(systemName: "checkmark")
            label.text = "Done"
            accessibilityLabel = "Finish dictation"
        case .transcribing:
            glyph.image = UIImage(systemName: "ellipsis")
            label.text = "Finishing"
            accessibilityLabel = "Finishing dictation"
        }
        isEnabled = phase != .transcribing
    }
}

/// A text or action key.
final class Key: UIControl {
    let output: String
    private var titleLabel: UILabel?
    private var symbolView: UIImageView?
    private var face: UIView!

    init(title: String? = nil, symbol: String? = nil, label: String? = nil, output: String? = nil, usesGlass: Bool = false) {
        self.output = output ?? title ?? ""
        super.init(frame: .zero)
        // The host hit-tests a remote keyboard surface. Give the touch cell a
        // continuous surface even where the inset keycap leaves a gutter.
        backgroundColor = UIColor(white: 0.5, alpha: 0.02)
        face = keyFace(radius: 8, in: self, usesGlass: usesGlass, inset: CGSize(width: 3, height: 5.5))
        let content: UIView
        if let symbol {
            let image = UIImageView(image: UIImage(systemName: symbol))
            image.tintColor = Ink.mark
            image.preferredSymbolConfiguration = .init(pointSize: 19, weight: .regular)
            symbolView = image
            content = image
        } else {
            let text = UILabel()
            text.text = title
            text.textColor = Ink.mark
            text.font = .systemFont(ofSize: (title?.count ?? 0) > 1 ? 18 : 22)
            titleLabel = text
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

    func setTitle(_ title: String) {
        titleLabel?.text = title
        accessibilityLabel = title
    }

    private var restingColor = Ink.key

    func setActionColor(_ accent: Bool) {
        restingColor = accent ? .systemBlue : Ink.key
        face.backgroundColor = restingColor
        symbolView?.tintColor = accent ? .white : Ink.mark
    }

    func setSymbol(_ symbol: String) {
        symbolView?.image = UIImage(systemName: symbol)
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    private var typingKeys: TypingKeys? {
        var ancestor = superview
        while let view = ancestor {
            if let keys = view as? TypingKeys { return keys }
            ancestor = view.superview
        }
        return nil
    }

    override var isHighlighted: Bool {
        didSet {
            guard oldValue != isHighlighted else { return }
            face.backgroundColor = isHighlighted ? Ink.keyDown : restingColor
            guard output.count == 1, let character = titleLabel?.text, character.count == 1,
                let keyboard = typingKeys else { return }
            if isHighlighted {
                keyboard.preview.show(character, for: self, in: keyboard)
            } else {
                keyboard.preview.hide(for: self)
            }
        }
    }
}

/// Reuse one material and label for all letters. Overlapping thumb presses
/// release independently, so only the key that owns the preview can hide it.
final class KeyPreview: UIControl {
    private let label = UILabel()
    private weak var owner: Key?

    override init(frame: CGRect) {
        super.init(frame: frame)
        isUserInteractionEnabled = false
        accessibilityElementsHidden = true
        _ = keyFace(radius: 12, in: self, usesGlass: true)
        label.textAlignment = .center
        label.textColor = Ink.mark
        label.font = .systemFont(ofSize: 29)
        addSubview(label)
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    func show(_ character: String, for key: Key, in keyboard: TypingKeys) {
        owner = key
        let rect = key.convert(key.bounds, to: keyboard)
        let width = max(46, rect.width + 4)
        let x = min(max(0, rect.midX - width / 2), keyboard.bounds.width - width)
        frame = CGRect(x: x, y: rect.minY - 40, width: width, height: 46)
        label.frame = bounds
        label.text = character
        keyboard.addSubview(self)
    }

    func hide(for key: Key) {
        if key === owner { removeFromSuperview() }
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

}
