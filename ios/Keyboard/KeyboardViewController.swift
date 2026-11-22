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
    private var manualEdit: ManualEdit?
    private var markedDraft: Draft?
    private var markWasVisible = false
    private let cancel = CloseButton()
    private let keys = TypingKeys()
    private lazy var typist = Typist { [completions, keys] in completions.suggestions(for: $0, trail: $1, layout: keys.keyLayout) }
    private var deleteRepeat: Timer?
    private var loggedField: UUID?
    private var pendingInsert = false
    private var activatedAt = Date.distantPast
    /// False once the field has failed to hold the words, for the rest of the take.
    private var wordsInField = true
    /// The field was sent mid-take, so hiding next is not the user walking away.
    private var takeWasSent = false
    private var currentTarget: DictationTarget? {
        let proxy = textDocumentProxy
        guard let field = SeeDocumentIdentifier(proxy) else { return nil }
        return DictationTarget(field: field, before: proxy.documentContextBeforeInput,
                               after: proxy.documentContextAfterInput, selected: proxy.selectedText, hasText: proxy.hasText)
    }

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
        keys.keyPrior = { [weak self] layout in
            guard let self, let input = WordInput(self.textDocumentProxy), self.typist.trail.end == Caret(self.textDocumentProxy),
                  let prior = self.completions.vocabulary.keyPrior(for: input, trail: self.typist.trail, layout: layout)
            else { return nil }
            return (prior, self.completions.vocabulary.spread, self.completions.vocabulary.offsets(for: layout))
        }
        typist.reviseSwipe = completions.vocabulary.reviseSwipe
        typist.reconsider = { [completions, keys] in completions.vocabulary.reconsider($0, input: $1, trail: $2, chosen: $3, layout: keys.keyLayout) }
        typist.editedInsideWord = completions.vocabulary.observeEditInsideWord
        typist.committed = { [completions, keys] in completions.vocabulary.learn($0, chosen: $1, picked: $2, layout: keys.keyLayout) }
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
        logEdit("hide") {}
        loggedField = nil
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
        markedDraft = nil
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
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.fieldSettles) { [weak self] in
            self?.startAfterReturningFromTheApp()
        }
    }

    private func configureTypingKeys() {
        let preference = TypingLanguages.current
        keys.configure(swedish: preference.usesSwedishKeys, needsGlobe: needsInputModeSwitchKey)
        completions.configure(preference)
        updateTypingContext()
    }

    private func updateTypingContext() {
        typist.checkSwipePosition(in: textDocumentProxy)
        keys.updateCase(for: textDocumentProxy)
        completions.update(for: textDocumentProxy, trail: typist.trail, layout: keys.keyLayout, canCorrect: typist.canCorrect(textDocumentProxy), swipeChoices: typist.swipeChoices(in: textDocumentProxy))
    }

    override func selectionDidChange(_ textInput: UITextInput?) {
        checkMark()
        checkForFinishedTake()
        updateTypingContext()
    }

    override func textWillChange(_ textInput: UITextInput?) {}

    override func textDidChange(_ textInput: UITextInput?) {
        switchAwayFromFieldsThatNeedTheirOwnKeys()
        _ = endTakeIfFieldWasSent()
        checkMark()
        checkForFinishedTake()
        updateTypingContext()
    }

    private func checkMark() {
        guard let draft = markedDraft else { return }
        if draft.placement(at: currentTarget, ownsMark: true) == .marked {
            markWasVisible = true
        } else {
            markedDraft = nil
        }
    }

    /// A chat app empties its field when it sends, live words included. Only the
    /// field can say so, because a keyboard never sees the send button.
    private func endTakeIfFieldWasSent() -> Bool {
        let proxy = textDocumentProxy
        guard state.phase == .recording || state.phase == .transcribing, wordsInField, !takeWasSent,
            let draft = Bridge.read(Draft.self, "draft.json"), let start = state.start,
            draft.start?.id == start.id, markWasVisible, ownsMark(draft),
            SeeDocumentIdentifier(proxy) == start.target.field, !proxy.hasText,
            proxy.selectedText?.isEmpty != false, proxy.documentContextBeforeInput?.isEmpty != false,
            proxy.documentContextAfterInput?.isEmpty != false
        else { return false }
        Diagnostics.log("sent_seen", ["phase": state.phase.rawValue, "chars": draft.text.count, "active": isActive])
        takeWasSent = true
        markedDraft = nil
        if isActive {
            proxy.setMarkedText("", selectedRange: NSRange(location: 0, length: 0))
            proxy.unmarkText()
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
        let previousStart = state.start?.id
        state = read ?? SessionState()
        if state.phase == .recording && state.start?.id != previousStart {
            wordsInField = true
            takeWasSent = false
        }
        if let id = state.start?.id, let edit = Bridge.read(ManualEdit.self, "manual-edit.json"), edit.startID == id {
            manualEdit = edit
            wordsInField = false
        } else {
            manualEdit = nil
        }
        if state.phase != previous {
            Diagnostics.log(
                "phase_seen",
                ["from": previous.rawValue, "to": state.phase.rawValue, "problem": state.problem ?? "none"])
        }
        pendingInsert = state.take.map {
            guard $0.sent != true, $0.id > (Bridge.read(Int64.self, "inserted.json") ?? 0),
                  let field = $0.start?.target.field else { return false }
            return field == SeeDocumentIdentifier(textDocumentProxy)
        } ?? false
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
        if state.phase != .recording, state.phase != .transcribing,
            let edit = Bridge.read(ManualEdit.self, "manual-edit.json"),
            edit.startID == nil || edit.startID != state.take?.start?.id
        {
            manualEdit = nil
            Bridge.remove("manual-edit.json")
        }
        // A take that ended with no transcript has no words to replace these with.
        if state.phase != .recording, state.phase != .transcribing,
            let draft = Bridge.read(Draft.self, "draft.json"), let start = draft.start,
            start.id != state.take?.start?.id, start.target.field == SeeDocumentIdentifier(textDocumentProxy)
        {
            if draft.dropped == true { _ = takeDraftBack(draft) }
            else if draft.placement(at: currentTarget, ownsMark: ownsMark(draft)) == .marked {
                textDocumentProxy.unmarkText()
            }
            markedDraft = nil
            Bridge.remove("draft.json")
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
        let preview = Bridge.read(DictationPreview.self, "words.json")
        let words = preview?.startID == state.start?.id ? preview?.words ?? "" : ""
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
        guard let start = state.start, start.target.selected == nil else { return false }
        let proxy = textDocumentProxy
        let current = currentTarget
        var before = proxy.documentContextBeforeInput
        if let last = Bridge.read(Draft.self, "draft.json"), last.start?.id == start.id {
            guard let placement = last.placement(at: current, ownsMark: ownsMark(last)) else { return false }
            guard last.words != words else { return true }
            if placement == .committed {
                guard let full = before else { return false }
                before = start.target.before == nil ? nil : String(full.dropLast(last.text.count))
                markedDraft = nil
                for _ in last.text { proxy.deleteBackward() }
            }
        } else {
            guard start.target.matches(current) else { return false }
        }
        let draft = Draft(start: start, before: before, spacing: Spacing.prefix(before: before), words: words)
        Bridge.write(draft, "draft.json")
        markedDraft = nil
        proxy.setMarkedText(draft.text, selectedRange: NSRange(location: (draft.text as NSString).length, length: 0))
        markedDraft = draft
        markWasVisible = draft.placement(at: currentTarget, ownsMark: true) != nil
        return true
    }

    private func takeDraftBack(_ draft: Draft) -> Bool {
        guard isActive, let placement = draft.placement(at: currentTarget, ownsMark: ownsMark(draft)) else { return false }
        markedDraft = nil
        let proxy = textDocumentProxy
        switch placement {
        case .committed:
            for _ in draft.text { proxy.deleteBackward() }
        case .marked:
            proxy.setMarkedText("", selectedRange: NSRange(location: 0, length: 0))
            proxy.unmarkText()
        }
        return true
    }

    private func ownsMark(_ draft: Draft) -> Bool {
        markedDraft?.start?.id == draft.start?.id && markedDraft?.text == draft.text
    }

    private func markDraftDropped() {
        guard var draft = Bridge.read(Draft.self, "draft.json"), let start = state.start,
            draft.start?.id == start.id else { return }
        draft.dropped = true
        Bridge.write(draft, "draft.json")
    }

    private func checkForFinishedTake() {
        guard isActive, fieldHasSettled, state.phase != .recording, state.phase != .transcribing,
            let take = state.take, let field = take.start?.target.field,
            field == SeeDocumentIdentifier(textDocumentProxy),
            take.id > (Bridge.read(Int64.self, "inserted.json") ?? 0) else { return }
        DispatchQueue.main.async { [weak self] in self?.settleField() }
    }

    /// Once per take, because the bookmark survives this process being replaced.
    private func insertFinishedTake() {
        guard isActive, state.phase != .recording, state.phase != .transcribing, let take = state.take else { return }
        let inserted = Bridge.read(Int64.self, "inserted.json") ?? 0
        guard take.id > inserted else { return }
        guard let start = take.start else {
            Bridge.write(take.id, "inserted.json")
            return
        }
        let draft = Bridge.read(Draft.self, "draft.json").flatMap { $0.start?.id == start.id ? $0 : nil }
        let edit = Bridge.read(ManualEdit.self, "manual-edit.json").flatMap { $0.startID == start.id ? $0 : nil }
        let now = Date()
        guard let current = currentTarget else { return }
        let delivery = take.delivery(to: current.field, at: now, hasDraft: draft != nil)
        guard delivery != .waitForField else { return }
        Bridge.write(take.id, "inserted.json")
        defer {
            if draft != nil { Bridge.remove("draft.json") }
            if edit != nil { Bridge.remove("manual-edit.json") }
        }
        guard delivery == .verifyTarget else { return }
        let proxy = textDocumentProxy
        if let draft {
            guard edit == nil, let placement = draft.placement(at: current, ownsMark: ownsMark(draft)) else { return }
            let text = draft.spacing + take.text
            if placement == .marked {
                markedDraft = nil
                proxy.setMarkedText(text, selectedRange: NSRange(location: (text as NSString).length, length: 0))
                proxy.unmarkText()
            } else {
                guard takeDraftBack(draft) else { return }
                proxy.insertText(text)
            }
        } else {
            if let edit {
                guard edit.target(for: start, at: current) != nil else { return }
            } else {
                guard start.target.matches(current) else { return }
            }
            let spacing = current.selected == nil ? Spacing.prefix(before: proxy.documentContextBeforeInput) : ""
            proxy.insertText(spacing + take.text)
        }
        Diagnostics.log("inserted", ["chars": take.text.count, "age_s": now.timeIntervalSince(take.finished), "draft": draft != nil])
    }

    private func startAfterReturningFromTheApp() {
        guard isActive, let handoff = Bridge.read(Handoff.self, "handoff.json") else { return }
        guard let start = handoff.start, Date().timeIntervalSince(handoff.asked) < Bridge.handoffWindow else {
            Bridge.remove("handoff.json")
            return
        }
        guard start.target.matches(currentTarget) else { return }
        whenAlive { [weak self] alive in
            guard let self, alive, self.isActive, self.state.phase == .armed,
                Bridge.read(Handoff.self, "handoff.json")?.start?.id == start.id,
                start.target.matches(self.currentTarget) else { return }
            Bridge.remove("handoff.json")
            self.startTake(start)
        }
    }

    private func startTake(_ start: DictationStart) {
        forgetTypingPosition()
        Bridge.write(start, "start.json")
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
        guard let target = currentTarget else { return }
        let start = DictationStart(id: UUID(), asked: Date(), target: target)
        // Asking first would only delay the app opening when the mic is known to be off.
        guard state.phase != .off else {
            Diagnostics.log("tap_app_off")
            openApp(start)
            return
        }
        whenAlive { [weak self] alive in
            guard let self, self.isActive, start.target.matches(self.currentTarget) else { return }
            guard alive else {
                Diagnostics.log("tap_app_dead", ["phase": self.state.phase.rawValue])
                self.openApp(start)
                return
            }
            if self.state.phase == .armed {
                Diagnostics.log("start_sent", ["by": "tap"])
                self.pressStartedTake = true
                self.startTake(start)
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

    private func openApp(_ start: DictationStart) {
        #if !STORE
        Bridge.write(Handoff(asked: start.asked, host: HostCapture.host, start: start), "handoff.json")
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
        #endif
        note = "Open see.computer to turn on the mic"
        render()
    }

    @objc private func openSettings() {
        #if !STORE
        var responder: UIResponder? = self
        while let current = responder {
            if let application = current as? UIApplication {
                application.open(URL(string: "app-settings:")!, options: [:], completionHandler: nil)
                return
            }
            responder = current.next
        }
        #endif
    }

    // MARK: Typing

    private func prepareForManualEdit() {
        guard state.phase == .recording || state.phase == .transcribing, let start = state.start,
            let current = currentTarget, start.target.field == current.field else { return }
        wordsInField = false
        let target: DictationTarget?
        if let edit = manualEdit {
            target = edit.target(for: start, at: current)
        } else if let draft = Bridge.read(Draft.self, "draft.json"), draft.start?.id == start.id {
            if takeDraftBack(draft) {
                Bridge.remove("draft.json")
                target = currentTarget
            } else {
                target = nil
            }
        } else {
            target = start.target.matches(current) ? current : nil
        }
        let edit = ManualEdit(startID: start.id, target: target)
        manualEdit = edit
        Bridge.write(edit, "manual-edit.json")
        showWords()
    }

    private func rememberEditedField() {
        guard state.phase == .recording || state.phase == .transcribing,
            var edit = manualEdit, let target = edit.target else { return }
        guard let current = currentTarget, target.field == current.field else { return }
        edit.target = current
        manualEdit = edit
        Bridge.write(edit, "manual-edit.json")
    }

    /// Logs an edit with the text before the caret on both sides of it, so a replay
    /// can see what each key, swipe or suggestion actually changed.
    private func logEdit(_ event: String, _ fields: [String: Any] = [:], _ edit: () -> Void) {
        let proxy = textDocumentProxy
        let before = proxy.documentContextBeforeInput
        edit()
        guard TypingLog.isOn else { return }
        logField()
        var fields = fields
        // Codes and passwords stay out, even though iOS rarely gives those fields to a custom keyboard.
        let secret = proxy.isSecureTextEntry == true || [.password, .newPassword, .oneTimeCode, .creditCardNumber]
            .contains(proxy.textContentType ?? .name)
        if secret {
            fields = ["private": true]
        } else {
            fields["b"] = before.map { String($0.suffix(80)) } ?? NSNull()
            fields["a"] = proxy.documentContextBeforeInput.map { String($0.suffix(80)) } ?? NSNull()
            fields["f"] = proxy.documentContextAfterInput.map { String($0.prefix(20)) } ?? NSNull()
        }
        TypingLog.write(event, fields)
    }

    /// What the field asks of a keyboard, and the key positions, once per field.
    private func logField() {
        let proxy = textDocumentProxy
        let field = SeeDocumentIdentifier(proxy)
        guard field != loggedField else { return }
        loggedField = field
        let layout = keys.keyLayout
        var centers: [String: [Double]] = [:]
        for (letter, code) in Lexicon.codes {
            if let center = layout.centers[code] { centers[String(letter)] = [Double(center.x), Double(center.y)] }
        }
        TypingLog.write("field", [
            "host": HostCapture.host ?? NSNull(),
            "keyboard": proxy.keyboardType?.rawValue ?? -1,
            "autocorrect": proxy.autocorrectionType?.rawValue ?? -1,
            "autocap": proxy.autocapitalizationType?.rawValue ?? -1,
            "spell": proxy.spellCheckingType?.rawValue ?? -1,
            "quotes": proxy.smartQuotesType?.rawValue ?? -1,
            "content": proxy.textContentType?.rawValue ?? NSNull(),
            "return": proxy.returnKeyType?.rawValue ?? -1,
            "corrects": WordInput.allowsCorrection(in: proxy),
            "languages": TypingLanguages.current.rawValue,
            "spread": completions.vocabulary.spread,
            "keys": centers,
            "pitch": Double(layout.pitch),
            "space": [layout.space.minX, layout.space.minY, layout.space.width, layout.space.height].map(Double.init),
            "size": [Double(keys.bounds.width), Double(keys.bounds.height)],
        ])
    }

    private func forgetTypingPosition() {
        typist.forget()
    }

    private func chooseSuggestion(_ input: WordInput, _ candidate: String) {
        guard isActive, keys.isUserInteractionEnabled, WordInput(textDocumentProxy) == input else { return }
        prepareForManualEdit()
        guard WordInput(textDocumentProxy) == input else { return }
        logEdit("pick", ["w": candidate, "s": completions.shownTitles]) { typist.choose(input, candidate, in: textDocumentProxy) }
        rememberEditedField()
        updateTypingContext()
    }

    private func typed(_ output: String, at point: CGPoint?) {
        guard isActive, keys.isUserInteractionEnabled else { return }
        prepareForManualEdit()
        var fields: [String: Any] = ["o": output, "s": completions.shownTitles]
        if let point { fields["x"] = Double(point.x); fields["y"] = Double(point.y) }
        logEdit("key", fields) { typist.type(output, at: point, in: textDocumentProxy) }
        rememberEditedField()
        updateTypingContext()
    }

    private func deleteCharacter() {
        guard isActive, keys.isUserInteractionEnabled else { return }
        prepareForManualEdit()
        logEdit("delete") { typist.delete(in: textDocumentProxy) }
        rememberEditedField()
        updateTypingContext()
    }

    private func deleteDown() {
        guard isActive, keys.isUserInteractionEnabled else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        deleteCharacter()
        deleteRepeat?.invalidate()
        deleteRepeat = Timer.scheduledTimer(withTimeInterval: TypingKeys.deleteRepeatDelay, repeats: false) { [weak self] _ in
            self?.deleteRepeat = Timer.scheduledTimer(withTimeInterval: TypingKeys.deleteRepeatInterval, repeats: true) { [weak self] _ in
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
            self?.logEdit("move", ["n": offset]) { self?.textDocumentProxy.adjustTextPosition(byCharacterOffset: offset) }
            self?.rememberEditedField()
            self?.updateTypingContext()
        }
        completions.accept = { [weak self] input, candidate in self?.chooseSuggestion(input, candidate) }
        keys.allowsSwipe = { [weak self] in
            guard let self else { return false }
            return WordInput.allowsCorrection(in: self.textDocumentProxy)
        }
        keys.onSwipe = { [weak self] samples, upper, locked, shortWord in
            guard let self, self.isActive, self.keys.isUserInteractionEnabled else { return false }
            self.prepareForManualEdit()
            guard WordInput.allowsCorrection(in: self.textDocumentProxy) else { return false }
            if shortWord != nil && !self.typist.startsSwipeWord(in: self.textDocumentProxy) { return false }
            let words = self.completions.vocabulary.swipe(samples, layout: self.keys.keyLayout,
                before: self.textDocumentProxy.documentContextBeforeInput ?? "", previousChoices: self.typist.precedingChoices(in: self.textDocumentProxy))
            let start = samples.first?.time ?? 0
            var fields: [String: Any] = [
                "p": samples.map { [Double($0.point.x), Double($0.point.y), ($0.time - start) * 1000] },
                "c": words.words.prefix(5).map { [$0.word, $0.score] as [Any] },
                "r": words.previous ?? NSNull(),
            ]
            if let shortWord { fields["short"] = shortWord }
            guard let best = words.words.first, shortWord == nil || best.word.lowercased() == shortWord else {
                self.logEdit("swipe", fields.merging(["kept": false]) { $1 }) {}
                return false
            }
            self.logEdit("swipe", fields) { self.typist.insertSwipe(words, uppercase: upper, locked: locked, in: self.textDocumentProxy) }
            self.rememberEditedField()
            self.updateTypingContext()
            return true
        }
        keys.insert = { [weak self] in self?.typed($0, at: $1) }
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
