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
    /// The user asked for a take while the speech model could not hear one.
    private var askedBeforeModel = false
    private var modelNote: String? {
        switch state.model {
        case .loading: return "The speech model is still loading"
        case .downloading: return "Finish the download in see.computer"
        case .ready, .failed, nil: return nil
        }
    }

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
    /// The take whose words this controller first marked, and when. A host that shows no
    /// marked text sends the field back unchanged within moments of that.
    private var marking: (take: UUID, since: Date, field: UUID?)?
    /// A finished take that went in as marked text, until the host has had its say.
    private var placing: Placing?
    private var placingDeadline: DispatchWorkItem?
    /// Finished takes the keyboard could not place for sure, oldest first, each with the
    /// words its field still lacks. The user can insert them in whichever field the keyboard
    /// is showing. An offer tied to its field would be lost with the field's identifier.
    private var offers: [(take: Take, words: String)] = []
    /// Typing puts the offer aside until the keyboard next comes up, so the suggestions return.
    private var offerSetAside = false
    /// Fields that a finished take is still waiting to be typed into.
    private var waitingFields: Set<UUID> = []
    /// A finished take left words in a field whose identifier has changed since. Any
    /// field the keyboard comes to may be that one.
    private var takeIsStranded = false
    /// The identifier the live take's field has now. It changes when the keyboard comes
    /// back to the field mid-take.
    private var takeField: UUID?
    /// An unreadable `state.json` lists no takes, which must not read as every take having ended.
    private var stateWasRead = false
    /// When this appearance committed a mark that an earlier keyboard left. See `strandedMarkIsCommitted`.
    private var strandedMarkCommitted: Date?
    private let offerIcon = UIImageView(image: UIImage(systemName: "text.insert"))
    private let offerButton = UIControl()
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
    /// How long a host gets to refuse marked text. The hosts measured answered in 10 to 50 ms.
    private static let hostAnswers: TimeInterval = 1
    /// How long a host gets to commit a mark before the keyboard edits the words in it.
    private static let commitTakes: TimeInterval = 0.5

    private struct Placing {
        let take: Take
        let field: UUID
        let text: String
        let before: String?
        let after: String?
    }

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
        typist.committed = { [completions, keys] in completions.vocabulary.learn($0, trail: $1, chosen: $2, picked: $3, layout: keys.keyLayout) }
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
                "draft": Ledger.read().entries.contains { $0.draft != nil },
            ])
        // A take goes on while the user looks elsewhere. The Dynamic Island shows it
        // and can finish it. Sending the words is the one way of leaving that ends it.
        if !endTakeIfFieldWasSent(), state.phase == .recording, !takeWasSent {
            Diagnostics.log("away_sent")
            Signals.post(.away)
        }
        markedDraft = nil
        settlePlacing()
        heartbeat?.invalidate()
        heartbeat = nil
        deleteUp()
        meter.stop()
    }

    private func activate() {
        guard !isActive else { return }
        isActive = true
        offerSetAside = false
        strandedMarkCommitted = nil
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
        _ = endTakeIfFieldWasSent()
        checkMark(textChanged: false)
        rememberEditedField(byHost: true)
        checkForFinishedTake()
        updateTypingContext()
    }

    override func textWillChange(_ textInput: UITextInput?) {}

    override func textDidChange(_ textInput: UITextInput?) {
        switchAwayFromFieldsThatNeedTheirOwnKeys()
        checkPlacing()
        _ = endTakeIfFieldWasSent()
        checkMark(textChanged: true)
        rememberEditedField(byHost: true)
        checkForFinishedTake()
        updateTypingContext()
    }

    /// Whether the host has had the time to refuse this controller's mark, and did not.
    private var markHeld: Bool {
        guard let marking, marking.take == state.start?.id else { return false }
        return Date().timeIntervalSince(marking.since) >= Self.hostAnswers
    }

    private func isIn(_ start: DictationStart) -> Bool {
        SeeDocumentIdentifier(textDocumentProxy) == takeField ?? start.target.field
    }

    private func checkMark(textChanged: Bool) {
        guard let draft = markedDraft else { return }
        let current = currentTarget
        let placement = draft.placement(at: current, before: textDocumentProxy.documentContextBeforeInput, ownsMark: true)
        let talking = state.phase == .recording || state.phase == .transcribing
        let start = state.start.flatMap { talking && $0.id == draft.start?.id && isIn($0) ? $0 : nil }
        // A host that shows no marked text answers the first mark by sending the field
        // back as it was. One that shows it sends nothing.
        let unanswered = textChanged && !markHeld
        if placement == .marked {
            if unanswered, let start { refuseMark(start) }
            return
        }
        // An emptied field is left for `endTakeIfFieldWasSent`, which needs the mark.
        guard current?.hasText != false else {
            if unanswered, let start, !start.target.hasText { refuseMark(start) }
            return
        }
        markedDraft = nil
        if placement == .committed, let id = draft.start?.id {
            var ledger = Ledger.read()
            ledger.update(id) { $0.draft?.shown = true }
            ledger.write()
        }
        // A caret moved in the host commits the marked words where they stand.
        if placement == nil, manualEdit == nil, !takeWasSent, let start { keepWordsInField(start) }
    }

    /// The host shows no marked text. The live words go to the keyboard's own row for the
    /// rest of the take, and none of them counts as being in the field.
    private func refuseMark(_ start: DictationStart) {
        Diagnostics.log("mark_refused", ["chars": markedDraft?.text.count ?? 0, "phase": state.phase.rawValue])
        markedDraft = nil
        if isActive {
            // A host that did hold the mark gives it up here, so the field is as it was either way.
            textDocumentProxy.setMarkedText("", selectedRange: NSRange(location: 0, length: 0))
            textDocumentProxy.unmarkText()
        }
        wordsInField = false
        save(ManualEdit(startID: start.id, target: currentTarget, kept: ""), for: start)
        showWords()
    }

    /// A chat app empties its field when it sends, live words included. Only the
    /// field can say so, because a keyboard never sees the send button.
    private func endTakeIfFieldWasSent() -> Bool {
        let proxy = textDocumentProxy
        guard state.phase == .recording || state.phase == .transcribing, !takeWasSent, let start = state.start,
            isIn(start), !proxy.hasText,
            proxy.selectedText?.isEmpty != false, proxy.documentContextBeforeInput?.isEmpty != false,
            proxy.documentContextAfterInput?.isEmpty != false
        else { return false }
        let draft = Ledger.read()[start.id]?.draft
        // Only words the host showed can have gone out with the field.
        let marked = draft.map { wordsInField && markHeld && ownsMark($0) } ?? false
        // After an edit the keyboard knows what the field held, so an empty field is the host's doing.
        let edited = manualEdit?.startID == start.id && manualEdit?.target?.hasText == true
        guard marked || edited else { return false }
        Diagnostics.log("sent_seen", ["phase": state.phase.rawValue, "chars": draft?.text.count ?? 0, "active": isActive])
        takeWasSent = true
        markedDraft = nil
        if isActive, marked {
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
        stateWasRead = read != nil
        let previous = state.phase
        let previousStart = state.start?.id
        state = read ?? SessionState()
        if state.phase == .recording && state.start?.id != previousStart {
            wordsInField = true
            takeWasSent = false
        }
        let ledger = Ledger.read()
        takeField = state.start.flatMap { ledger[$0.id]?.home }
        if let id = state.start?.id, let edit = ledger[id]?.edit {
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
        waitingFields = fieldsWaiting(in: ledger)
        pendingInsert = SeeDocumentIdentifier(textDocumentProxy).map(waitingFields.contains) ?? false
        if fieldHasSettled { startAfterReturningFromTheApp() }
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
        guard strandedMarkIsCommitted() else {
            DispatchQueue.main.asyncAfter(deadline: .now() + Self.commitTakes) { [weak self] in self?.settleField() }
            return
        }
        deliverFinishedTakes()
        pendingInsert = false
        clearEndedTakes()
        showWords()
        updateTypingContext()
        render()
    }

    private var fieldHasSettled: Bool { Date().timeIntervalSince(activatedAt) >= Self.fieldSettles }

    /// A keyboard that went away mid-take can leave its words marked in the field. To the
    /// keyboard that comes back they read as plain text, but one delete removes the whole
    /// marked run, and the next key typed replaces it. So the mark is committed first, in
    /// a turn of its own, because the host can apply an edit sent in the same turn before it.
    private func strandedMarkIsCommitted() -> Bool {
        if let strandedMarkCommitted { return Date().timeIntervalSince(strandedMarkCommitted) >= Self.commitTakes }
        let waiting = Set((state.takes.map(\.start) + [state.start]).compactMap { $0?.id })
        let stranded = Ledger.read().entries.contains { $0.standing == nil && $0.draft != nil && waiting.contains($0.id) }
        guard markedDraft == nil, stranded else {
            strandedMarkCommitted = .distantPast
            return true
        }
        textDocumentProxy.unmarkText()
        strandedMarkCommitted = Date()
        return false
    }

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
        if wordsInField, state.phase == .recording, !takeWasSent, isActive, fieldHasSettled, strandedMarkIsCommitted(),
            !words.isEmpty, !putInField(words)
        {
            Diagnostics.log("words_left_field", ["chars": words.count])
            if manualEdit == nil, let start = state.start, start.target.selected == nil, isIn(start) {
                keepWordsInField(start)
            }
            wordsInField = false
            render()
        }
        liveWords = wordsInField ? "" : Transcript.rest(of: words, after: manualEdit?.kept ?? "")
        if !wordsInField { render() }
    }

    private func putInField(_ words: String) -> Bool {
        guard let start = state.start, start.target.selected == nil else { return false }
        let proxy = textDocumentProxy
        let current = currentTarget
        var before = proxy.documentContextBeforeInput
        var spacing = Spacing.prefix(before: before)
        var shown = false
        var ledger = Ledger.read()
        if let last = ledger[start.id]?.draft {
            let free = current.map { ledger.isFree($0.field, for: start.id) } ?? false
            guard let placement = last.placement(at: current, before: before, ownsMark: ownsMark(last), mayHaveMoved: free)
            else { return false }
            // A host that shows no marked text says so within moments of the first mark.
            shown = last.shown == true || placement == .committed || markHeld
            if last.words == words {
                if shown, last.shown != true {
                    ledger.update(start.id) { $0.draft?.shown = true }
                    ledger.write()
                }
                return true
            }
            spacing = last.spacing
            if placement == .committed {
                guard let full = before else { return false }
                before = start.target.before == nil ? nil : String(full.dropLast(last.text.count))
                markedDraft = nil
                for _ in last.text { proxy.deleteBackward() }
            }
        } else {
            guard start.target.matches(current) else { return false }
        }
        var draft = Draft(start: start, before: before, spacing: spacing, words: words)
        draft.shown = shown ? true : nil
        draft.field = current?.field
        takeField = current?.field
        // Saved before the host is asked, so a keyboard replaced mid-take can still find the
        // words. Being saved does not make them shown.
        ledger.update(start.id) {
            $0.draft = draft
            $0.field = current?.field
        }
        ledger.write()
        markedDraft = nil
        proxy.setMarkedText(draft.text, selectedRange: NSRange(location: (draft.text as NSString).length, length: 0))
        markedDraft = draft
        // A field the keyboard has just come back to is a new host session to answer the mark.
        if marking?.take != start.id || marking?.field != current?.field { marking = (start.id, Date(), current?.field) }
        return true
    }

    private func takeDraftBack(_ draft: Draft) -> Bool {
        let proxy = textDocumentProxy
        guard isActive,
            let placement = draft.placement(at: currentTarget, before: proxy.documentContextBeforeInput, ownsMark: ownsMark(draft))
        else { return false }
        markedDraft = nil
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
        guard let start = state.start else { return }
        var ledger = Ledger.read()
        guard ledger[start.id]?.draft != nil else { return }
        ledger.update(start.id) { $0.draft?.dropped = true }
        ledger.write()
    }

    private func checkForFinishedTake() {
        guard isActive, fieldHasSettled, state.phase != .recording, state.phase != .transcribing, placing == nil,
            let field = SeeDocumentIdentifier(textDocumentProxy), waitingFields.contains(field) || takeIsStranded
        else { return }
        DispatchQueue.main.async { [weak self] in self?.settleField() }
    }

    /// The fields of finished takes that the keyboard has not tried to place yet.
    private func fieldsWaiting(in ledger: Ledger) -> Set<UUID> {
        Set(state.takes.compactMap { take in
            guard take.sent != true, let start = take.start, !ledger.isSettled(take),
                ledger[start.id]?.standing == nil else { return nil }
            return ledger[start.id]?.home ?? start.target.field
        })
    }

    /// Each finished take goes into its own field once. One the keyboard cannot place
    /// for sure stays with the user as an offer, and none is typed a second time.
    private func deliverFinishedTakes() {
        guard isActive, state.phase != .recording, state.phase != .transcribing, placing == nil else { return }
        let saved = Ledger.read()
        guard let current = currentTarget else {
            // iOS has not attached the field yet, and a take that waits should not have to
            // wait for the app's next signal as well.
            if state.takes.contains(where: { !saved.isSettled($0) }) {
                DispatchQueue.main.asyncAfter(deadline: .now() + Self.fieldSettles) { [weak self] in self?.settleField() }
            }
            return
        }
        var ledger = saved
        let now = Date()
        offers = []
        for take in state.takes where !ledger.isSettled(take) {
            guard let start = take.start else { continue }
            let entry = ledger[start.id]
            // Words in the field, and a take the keyboard has already tried to place, are
            // worth coming back to for longer.
            let lasting = entry?.draft != nil || entry?.standing != nil
            var delivery = take.delivery(to: current.field, at: now, hasDraft: lasting, home: entry?.home)
            // The keyboard has come back to the field under a new identifier, and the words
            // the take left there say which field it is.
            let before = textDocumentProxy.documentContextBeforeInput
            if delivery == .waitForField, entry?.standing == nil, let draft = entry?.draft,
                ledger.isFree(current.field, for: start.id)
            {
                if draft.placement(at: current, before: before, ownsMark: false, mayHaveMoved: true) == .committed {
                    delivery = .verifyTarget
                } else if let kept = draft.wordsLeft(at: current, before: before) {
                    // Too little to edit the field on. The user is offered what it lacks.
                    Diagnostics.log("unplaced", ["why": "field", "chars": take.text.count, "kept": kept.count])
                    ledger.update(start.id) {
                        $0.draft = nil
                        $0.edit = ManualEdit(startID: start.id, target: nil, kept: kept)
                        $0.field = current.field
                        $0.standing = .offered
                    }
                }
            }
            if delivery == .historyOnly {
                ledger.remove(start.id)
                continue
            }
            if delivery == .verifyTarget, entry?.standing == nil {
                place(take, entry, at: current, in: &ledger)
            } else if entry?.standing == .sent {
                // The keyboard that sent the take did not last to hear the host's answer. The
                // field as the host reports it now decides, and the take is not sent again.
                let landed = ledger.shows(take, in: current.field, isOwn: delivery == .verifyTarget, before: before)
                Diagnostics.log(landed ? "placed" : "unplaced", ["why": "restart"])
                settle(start.id, as: landed ? .placed : .offered, in: &ledger, field: landed ? current.field : nil)
            }
            if placing != nil { break }
            guard let offered = ledger[start.id], offered.standing == .offered else { continue }
            // Words the user kept in the field mid-take are there already.
            let words = Transcript.rest(of: take.text, after: offered.edit?.kept ?? "")
            if words.isEmpty {
                settle(start.id, as: .placed, in: &ledger)
            } else {
                offers.append((take, words))
            }
        }
        if ledger != saved { ledger.write() }
        waitingFields = fieldsWaiting(in: ledger)
        takeIsStranded = state.takes.contains { take in
            guard let id = take.start?.id, let entry = ledger[id] else { return false }
            return entry.standing == nil && entry.draft != nil && entry.home != current.field
        }
    }

    /// `field` is where the take's words went. It stays with the settled take, so words
    /// there are not taken for another take's draft.
    private func settle(_ id: UUID, as standing: Ledger.Standing, in ledger: inout Ledger, field: UUID? = nil) {
        ledger.update(id) {
            $0.standing = standing
            $0.field = field ?? $0.field
            guard standing == .placed || standing == .dismissed else { return }
            $0.draft = nil
            $0.edit = nil
        }
    }

    /// Checks that the field is where the take left it, then types the take. Nothing is
    /// typed into a field, a selection or a place the keyboard cannot recognise.
    private func place(_ take: Take, _ entry: Ledger.Entry?, at current: DictationTarget, in ledger: inout Ledger) {
        guard let start = take.start else { return }
        let proxy = textDocumentProxy
        let before = proxy.documentContextBeforeInput
        let age = Date().timeIntervalSince(take.finished)
        func offer(_ why: String) {
            Diagnostics.log("unplaced", ["why": why, "chars": take.text.count, "age_s": age])
            settle(start.id, as: .offered, in: &ledger, field: current.field)
        }
        // The ledger says the take was sent before the host hears of it, so a keyboard
        // killed in between finds the take sent and does not type it again.
        func send(_ how: String, _ edits: () -> Void) {
            settle(start.id, as: .sent, in: &ledger, field: current.field)
            ledger.write()
            edits()
            Diagnostics.log("inserted", [
                "chars": take.text.count, "age_s": age, "draft": entry?.draft != nil,
                "kept": entry?.edit?.kept?.count ?? 0, "how": how,
            ])
        }
        if let draft = entry?.draft {
            let free = ledger.isFree(current.field, for: start.id)
            guard entry?.edit == nil,
                let placement = draft.placement(at: current, before: before, ownsMark: ownsMark(draft), mayHaveMoved: free)
            else { return offer("draft") }
            let text = draft.spacing + take.text
            markedDraft = nil
            switch placement {
            case .marked:
                send("mark") {
                    proxy.setMarkedText(text, selectedRange: NSRange(location: (text as NSString).length, length: 0))
                    proxy.unmarkText()
                }
                awaitHost(Placing(
                    take: take, field: current.field, text: text, before: before, after: proxy.documentContextAfterInput))
                return
            case .committed where draft.text != text:
                send("replace") {
                    for _ in draft.text { proxy.deleteBackward() }
                    proxy.insertText(text)
                }
            case .committed:
                Diagnostics.log("inserted", ["chars": take.text.count, "age_s": age, "draft": true, "how": "as_shown"])
            }
        } else {
            let edit = entry?.edit
            guard edit.map({ $0.target(for: start, at: current) != nil }) ?? start.target.matches(current)
            else { return offer("target") }
            let rest = Transcript.rest(of: take.text, after: edit?.kept ?? "")
            if !rest.isEmpty {
                send("plain") { proxy.insertText(spaced(rest, replacingSelection: current.selected != nil)) }
            }
        }
        settle(start.id, as: .placed, in: &ledger, field: current.field)
    }

    /// A take's words as they go in at the caret, spaced from the text on both sides.
    private func spaced(_ words: String, replacingSelection: Bool) -> String {
        let proxy = textDocumentProxy
        let spacing = replacingSelection ? "" : Spacing.prefix(before: proxy.documentContextBeforeInput)
        // The caret can sit before text the user moved it in front of.
        let next = proxy.documentContextAfterInput?.first
        let separator = next.map { !$0.isWhitespace && !".,!?;:)]}…’”".contains($0) } == true ? " " : ""
        return spacing + words + separator
    }

    /// Marked text is the one edit a host answers. A host that takes none sends the field
    /// back as it was, within moments, and the take then goes in as plain text.
    private func awaitHost(_ placing: Placing) {
        self.placing = placing
        let deadline = DispatchWorkItem { [weak self] in self?.settlePlacing() }
        placingDeadline = deadline
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.hostAnswers, execute: deadline)
    }

    private func checkPlacing() {
        guard let placing, let id = placing.take.start?.id else { return }
        let proxy = textDocumentProxy
        guard HostContext.refused(
            placing.text, before: placing.before, after: placing.after,
            now: proxy.documentContextBeforeInput, nowAfter: proxy.documentContextAfterInput)
        else { return settlePlacing(hostAnswered: true) }
        self.placing = nil
        placingDeadline?.cancel()
        Diagnostics.log("mark_refused", ["chars": placing.text.count, "phase": state.phase.rawValue])
        var ledger = Ledger.read()
        guard ledger[id]?.standing == .sent else { return }
        if isActive, SeeDocumentIdentifier(proxy) == placing.field {
            proxy.insertText(placing.text)
            Diagnostics.log("inserted", ["chars": placing.take.text.count, "how": "plain_after_refusal"])
            settle(id, as: .placed, in: &ledger, field: placing.field)
        } else {
            Diagnostics.log("unplaced", ["why": "refused"])
            settle(id, as: .offered, in: &ledger, field: placing.field)
        }
        ledger.write()
        DispatchQueue.main.async { [weak self] in self?.settleField() }
    }

    /// The host raised no objection in time, or the user typed on, or the keyboard is
    /// leaving. A refusal that comes after any of those can no longer be acted on.
    private func settlePlacing(hostAnswered: Bool = false) {
        guard let placing, let id = placing.take.start?.id else { return }
        self.placing = nil
        placingDeadline?.cancel()
        var ledger = Ledger.read()
        guard ledger[id]?.standing == .sent else { return }
        settle(id, as: .placed, in: &ledger, field: placing.field)
        ledger.write()
        Diagnostics.log("placed", ["host": hostAnswered ? "answered" : "quiet"])
        guard isActive else { return }
        DispatchQueue.main.async { [weak self] in self?.settleField() }
    }

    /// A take that ended with no transcript left its words in the field and nothing will
    /// come to replace them. Words in another field wait there for the user to come back.
    private func clearEndedTakes() {
        guard stateWasRead, state.phase != .recording, state.phase != .transcribing else { return }
        var ledger = Ledger.read()
        let listed = Set(state.takes.compactMap { $0.start?.id })
        let field = SeeDocumentIdentifier(textDocumentProxy)
        var cleared = false
        for entry in ledger.entries where !listed.contains(entry.id) {
            if let draft = entry.draft, entry.standing == nil {
                guard draft.home == field else { continue }
                let before = textDocumentProxy.documentContextBeforeInput
                if draft.dropped == true {
                    _ = takeDraftBack(draft)
                } else if draft.placement(at: currentTarget, before: before, ownsMark: ownsMark(draft)) == .marked {
                    textDocumentProxy.unmarkText()
                }
                markedDraft = nil
            }
            ledger.remove(entry.id)
            cleared = true
        }
        guard cleared else { return }
        ledger.write()
        if let id = manualEdit?.startID, ledger[id] == nil { manualEdit = nil }
    }

    // MARK: A take the user inserts by hand

    private var shownOffer: (take: Take, words: String)? {
        guard !offerSetAside, state.phase != .recording, state.phase != .transcribing,
            SeeDocumentIdentifier(textDocumentProxy) != nil else { return nil }
        return offers.first
    }

    @objc private func offerTapped() {
        guard isActive, let (take, words) = shownOffer, let id = take.start?.id else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        var ledger = Ledger.read()
        guard ledger[id]?.standing == .offered else { return settleField() }
        // Settled before the words go in, so no keyboard that comes after offers them again.
        settle(id, as: .placed, in: &ledger, field: SeeDocumentIdentifier(textDocumentProxy))
        ledger.write()
        forgetTypingPosition()
        logEdit("offer") {
            textDocumentProxy.insertText(spaced(words, replacingSelection: textDocumentProxy.selectedText?.isEmpty == false))
        }
        Diagnostics.log("inserted", [
            "chars": words.count, "age_s": Date().timeIntervalSince(take.finished), "how": "hand",
        ])
        settleField()
    }

    private func dismissOffer() {
        guard let take = shownOffer?.take, let id = take.start?.id else { return }
        var ledger = Ledger.read()
        settle(id, as: .dismissed, in: &ledger)
        ledger.write()
        Diagnostics.log("offer_dismissed", ["chars": take.text.count])
        settleField()
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
            // The next state the app publishes brings this back, within the handoff's 15 seconds.
            guard self.modelNote == nil else {
                self.askedBeforeModel = true
                self.render()
                return
            }
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
        askedBeforeModel = false
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
                guard self.modelNote == nil else {
                    Diagnostics.log("tap_before_model", ["model": self.state.model?.rawValue ?? "none"])
                    self.askedBeforeModel = true
                    self.render()
                    return
                }
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
        guard shownOffer == nil else { return dismissOffer() }
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
        settlePlacing()
        if !offers.isEmpty, !offerSetAside {
            offerSetAside = true
            render()
        }
        guard state.phase == .recording || state.phase == .transcribing, let start = state.start,
            let current = currentTarget, isIn(start) else { return }
        if var edit = manualEdit {
            // After Done, an edit the keyboard did not make leaves the insertion point unknown.
            if state.phase == .transcribing, edit.target(for: start, at: current) == nil, edit.target != nil {
                edit.target = nil
                save(edit, for: start)
            }
        } else {
            keepWordsInField(start)
        }
        showWords()
    }

    private func save(_ edit: ManualEdit, for start: DictationStart) {
        manualEdit = edit
        takeField = edit.target?.field ?? takeField
        var ledger = Ledger.read()
        ledger.update(start.id) {
            $0.draft = nil
            $0.edit = edit
            $0.field = edit.target?.field ?? $0.field
        }
        ledger.write()
    }

    /// The words shown so far stay in the field as the user's own text, and the rest
    /// of the take lands at the caret when it ends. Nothing the user saw is taken back.
    private func keepWordsInField(_ start: DictationStart) {
        wordsInField = false
        let proxy = textDocumentProxy
        var kept = ""
        var known = state.phase == .recording
        if var draft = Ledger.read()[start.id]?.draft {
            let owned = ownsMark(draft)
            let placement = draft.placement(at: currentTarget, before: proxy.documentContextBeforeInput, ownsMark: owned)
            if owned, markHeld { draft.shown = true }
            markedDraft = nil
            let retyped = isActive && placement == .marked
            if retyped {
                // Typed in again as plain text, so the context the next key reads holds them.
                proxy.setMarkedText("", selectedRange: NSRange(location: 0, length: 0))
                proxy.unmarkText()
                proxy.insertText(draft.text)
                typist.startNewWord(in: proxy)
            } else if isActive {
                // Some hosts already committed them. Others still hold them marked, and the next key would replace them.
                proxy.unmarkText()
            }
            kept = draft.kept(placement, retyped: retyped)
            known = known || placement != nil
        } else {
            known = known || start.target.matches(currentTarget)
        }
        Diagnostics.log("words_kept", ["chars": kept.count, "phase": state.phase.rawValue, "known": known])
        save(ManualEdit(startID: start.id, target: known ? currentTarget : nil, kept: kept), for: start)
    }

    /// While recording, wherever the user leaves the caret in the take's field is where
    /// the rest goes. After Done, only the keyboard's own edits keep that point known.
    private func rememberEditedField(byHost: Bool = false) {
        guard state.phase == .recording || state.phase == .transcribing, var edit = manualEdit,
            let start = state.start, edit.startID == start.id,
            let current = currentTarget, isIn(start) else { return }
        if state.phase == .transcribing {
            guard !byHost, edit.target != nil else { return }
        } else if byHost, !current.hasText {
            // An emptied field is a sent message, which `endTakeIfFieldWasSent` handles.
            return
        }
        edit.target = current
        save(edit, for: start)
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
        logEdit("key", fields) { typist.type(output, at: point, in: textDocumentProxy, layout: keys.keyLayout) }
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
        // An answer to the last tap comes first. Words waiting to be inserted come before a standing problem.
        let reply = hasFullAccess ? (askedBeforeModel ? modelNote : nil) ?? note : "Allow Full Access"
        let offered = talking || reply != nil ? nil : shownOffer
        let message = reply ?? state.problem
        bar.show(phase)
        let preview = !wordsInField && !liveWords.isEmpty
        let elapsed = max(0, Int(Date().timeIntervalSince(state.since ?? Date())))
        if talking {
            status.text = preview ? liveWords : String(format: "%d:%02d", elapsed / 60, elapsed % 60)
        } else {
            status.text = offered.map { "Insert “\($0.words)”" } ?? message
        }
        status.font = preview || !talking ? .systemFont(ofSize: 15) : .monospacedDigitSystemFont(ofSize: 15, weight: .medium)
        status.textColor = offered == nil ? .secondaryLabel : .label
        // Live words show their newest end. Words to insert show how they begin.
        status.lineBreakMode = offered == nil ? .byTruncatingHead : .byTruncatingTail
        offerIcon.isHidden = offered == nil
        offerButton.isHidden = offered == nil
        statusRow.isHidden = !talking && message == nil && offered == nil
        completions.isHidden = !statusRow.isHidden
        meter.isHidden = phase != .recording || preview
        if meter.isHidden { meter.stop() } else { meter.start() }
        cancel.isHidden = phase != .recording && offered == nil
        cancel.accessibilityLabel = offered == nil ? "Cancel" : "Dismiss dictation"
        keys.isUserInteractionEnabled = !pendingInsert
        keys.accessibilityElementsHidden = pendingInsert
        if !keys.isUserInteractionEnabled { deleteUp() }
    }

    private func layout() {
        guard let root = view else { return }
        // iOS pins a custom keyboard's bottom above its own globe row and usually draws 17 pt of
        // keyboard above the view. 757 px starts the view at the top of Apple's suggestion strip on
        // an iPhone 17 Pro. The extra inset is the touch margin under the bottom row's caps.
        let height = root.heightAnchor.constraint(equalToConstant: 757.0 / 3 + Key.capInset)
        height.priority = .defaultHigh
        height.isActive = true
        root.clipsToBounds = true
        bar.addTarget(self, action: #selector(micDown), for: .touchDown)
        bar.addTarget(self, action: #selector(micUp), for: [.touchUpInside, .touchUpOutside, .touchCancel])
        statusRow.alignment = .center
        statusRow.spacing = 8
        status.lineBreakMode = .byTruncatingHead
        status.textColor = .secondaryLabel
        offerIcon.tintColor = .label
        offerIcon.preferredSymbolConfiguration = .init(pointSize: 15, weight: .medium)
        offerIcon.setContentHuggingPriority(.required, for: .horizontal)
        offerIcon.isHidden = true
        statusRow.addArrangedSubview(meter)
        statusRow.addArrangedSubview(offerIcon)
        statusRow.addArrangedSubview(status)
        offerButton.addTarget(self, action: #selector(offerTapped), for: .touchUpInside)
        offerButton.isAccessibilityElement = true
        offerButton.accessibilityTraits = .button
        offerButton.accessibilityLabel = "Insert dictation"
        offerButton.isHidden = true
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
        keys.prepareSwipe = { [weak self] in self?.completions.vocabulary.prepareSwipe() }
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
        for child in [completions, statusRow, offerButton, controls, keys] {
            child.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(child)
        }
        NSLayoutConstraint.activate([
            // The rows take their full height from the bottom, and the strip and Dictate bar get the rest.
            // iOS sometimes leaves out the 17 pt above the view, for example after the emoji keyboard,
            // and the extension cannot tell. The gap keeps the Dictate bar off the edge then.
            controls.topAnchor.constraint(equalTo: root.topAnchor, constant: 4),
            controls.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -12),

            bar.widthAnchor.constraint(equalToConstant: 108),
            completions.topAnchor.constraint(equalTo: controls.topAnchor, constant: CompletionStrip.textDrop),
            completions.bottomAnchor.constraint(equalTo: controls.bottomAnchor, constant: CompletionStrip.textDrop),
            completions.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 7),
            completions.trailingAnchor.constraint(equalTo: controls.leadingAnchor, constant: -6),
            statusRow.centerYAnchor.constraint(equalTo: controls.centerYAnchor),
            statusRow.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 14),
            statusRow.trailingAnchor.constraint(lessThanOrEqualTo: controls.leadingAnchor, constant: -6),
            // The whole strip is the target, because the row's own text is only a line high.
            offerButton.topAnchor.constraint(equalTo: controls.topAnchor),
            offerButton.bottomAnchor.constraint(equalTo: controls.bottomAnchor),
            offerButton.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            offerButton.trailingAnchor.constraint(equalTo: controls.leadingAnchor),
            keys.topAnchor.constraint(equalTo: controls.bottomAnchor, constant: 11.0 / 6),
            keys.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 11.0 / 3),
            keys.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -11.0 / 3),
            keys.bottomAnchor.constraint(equalTo: root.bottomAnchor)
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
