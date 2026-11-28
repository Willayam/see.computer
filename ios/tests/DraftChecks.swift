import Foundation

@main
enum DraftChecks {
    static let field = UUID()
    static let other = UUID()
    static let encoder = JSONEncoder()
    static let decoder = JSONDecoder()

    static func target(_ before: String?, after: String? = "next", selected: String? = nil,
                       field id: UUID? = nil, hasText: Bool = true) -> DictationTarget {
        DictationTarget(field: id ?? field, before: before, after: after, selected: selected, hasText: hasText)
    }

    static func tap(at target: DictationTarget) -> DictationStart {
        DictationStart(id: UUID(), asked: Date(), target: target)
    }

    /// The value as the next keyboard or app process reads it back from its file.
    static func reopened<Value: Codable>(_ value: Value) throws -> Value {
        try decoder.decode(Value.self, from: encoder.encode(value))
    }

    /// Where a draft stands when the host reports `before` as the text before the caret.
    static func placement(_ draft: Draft, _ before: String?, after: String? = "next", selected: String? = nil,
                          field id: UUID? = nil, hasText: Bool = true, ownsMark: Bool,
                          mayHaveMoved: Bool = false) -> Draft.Placement? {
        draft.placement(
            at: target(before, after: after, selected: selected, field: id, hasText: hasText), before: before,
            ownsMark: ownsMark, mayHaveMoved: mayHaveMoved)
    }

    static func main() throws {
        try draftsAndEdits()
        try savedShapes()
        transcripts()
        attemptedWordsAreNotKept()
        hostsThatReportOnlyTheEnd()
        refusals()
        try everyWaitingTakeIsKept()
        try theLedgerSettlesEachTakeOnce()
        aTakeSentBeforeARestart()
        try whatAnOlderKeyboardLeftBehind()
        print("Draft checks passed")
    }

    static func draftsAndEdits() throws {
        let origin = target("hello")
        let start = tap(at: origin)
        let draft = Draft(start: start, before: "hello", spacing: " ", words: "a long preview ending in words")
        let full = "hello" + draft.text

        precondition(!origin.matches(nil))
        precondition(draft.placement(at: nil, before: nil, ownsMark: true) == nil)
        precondition(origin.matches(target("hello")))
        precondition(!origin.matches(target("hello", field: other)))
        precondition(!origin.matches(target("hello", selected: "next")))
        precondition(!origin.matches(target("hello", after: "changed")))
        precondition(placement(draft, "hello", ownsMark: true) == .marked)
        precondition(placement(draft, "hello", ownsMark: false) == nil)
        precondition(placement(draft, full, ownsMark: false) == .committed)
        precondition(placement(draft, "hello", field: other, ownsMark: true) == nil, "A mark belongs to the field it was set in")
        precondition(placement(draft, "words", ownsMark: true) == nil, "A word the field shares with the draft proves nothing")
        precondition(placement(draft, full, after: "changed", ownsMark: true) == nil)
        precondition(placement(draft, full, selected: "next", ownsMark: true) == nil)
        precondition(placement(draft, full + " extra", ownsMark: true) == nil)

        let empty = target(nil, after: nil, hasText: false)
        let emptyStart = tap(at: empty)
        let emptyDraft = Draft(start: emptyStart, before: nil, spacing: "", words: "first words")
        precondition(placement(emptyDraft, nil, after: nil, ownsMark: true) == .marked)
        precondition(placement(emptyDraft, "first words", after: nil, ownsMark: false) == .committed)
        precondition(placement(emptyDraft, nil, after: nil, hasText: false, ownsMark: true) == nil)

        let selected = target("hello", selected: "word")
        precondition(selected.matches(target("hello", selected: "word")))
        precondition(!selected.matches(origin))
        let unreadable = target(nil, after: nil)
        precondition(!unreadable.matches(unreadable))

        let edited = target("hello typed")
        let finished = Take(id: 1, text: "final dictation", finished: Date(), start: start)
        let returned = finished.finished.addingTimeInterval(30)
        precondition(finished.delivery(to: other, at: finished.finished, hasDraft: false) == .waitForField)
        precondition(finished.delivery(to: field, at: finished.finished, hasDraft: false) == .verifyTarget)
        precondition(finished.delivery(to: field, at: returned, hasDraft: false) == .historyOnly)
        precondition(finished.delivery(to: other, at: returned, hasDraft: true) == .waitForField)
        precondition(finished.delivery(to: field, at: returned, hasDraft: true) == .verifyTarget)
        var sent = finished
        sent.sent = true
        precondition(sent.delivery(to: field, at: sent.finished, hasDraft: true) == .historyOnly)

        let edit = ManualEdit(startID: start.id, target: edited)
        let savedTake = try reopened(finished)
        let savedEdit = try reopened(edit)
        precondition(savedEdit.target(for: savedTake.start!, at: edited) == edited)
        precondition(savedEdit.target(for: emptyStart, at: edited) == nil)
        precondition(savedEdit.target(for: start, at: origin) == nil)
        precondition(savedEdit.target(for: start, at: target("hello typed", field: other)) == nil)
        precondition(finished.delivery(to: other, at: finished.finished, hasDraft: false, home: other) == .verifyTarget)
        precondition(finished.delivery(to: field, at: finished.finished, hasDraft: false, home: other) == .waitForField)
        precondition(savedEdit.target(for: start, at: target("hello typed", selected: "next")) == nil)
        precondition(ManualEdit(startID: start.id, target: nil).target(for: start, at: edited) == nil)
        precondition(savedEdit.kept == nil)
        let keptEdit = try reopened(ManualEdit(startID: start.id, target: edited, kept: "a long preview"))
        precondition(keptEdit.kept == "a long preview")
    }

    static func savedShapes() throws {
        let start = tap(at: target("hello"))
        let handoffData = try encoder.encode(Handoff(asked: start.asked, start: start))
        let savedHandoff = try decoder.decode(Handoff.self, from: handoffData)
        precondition(savedHandoff.start?.target.matches(target("hello")) == true)
        precondition(savedHandoff.start?.target.matches(target("hello", field: other)) == false)
        precondition(!String(decoding: handoffData, as: UTF8.self).contains("hello"))

        let oldHandoff = try decoder.decode(Handoff.self, from: Data(#"{"asked":0}"#.utf8))
        let oldDraft = try decoder.decode(Draft.self, from: Data(#"{"before":"hello","spacing":" ","words":"old"}"#.utf8))
        let oldEdit = try decoder.decode(ManualEdit.self, from: Data("""
            {"since":0,"field":"\(field)","previewStillInField":true}
            """.utf8))
        precondition(oldHandoff.start == nil)
        precondition(placement(oldDraft, "hello old", ownsMark: true) == nil)
        precondition(oldEdit.startID == nil && oldEdit.target == nil && oldEdit.kept == nil)

        var loading = SessionState(phase: .armed)
        loading.model = .loading
        let savedLoading = try reopened(loading)
        precondition(savedLoading.model == .loading && savedLoading.since == nil)
    }

    static func transcripts() {
        let final = "Hello world, this is a test of the keyboard."
        precondition(Transcript.rest(of: final, after: "") == final)
        precondition(Transcript.rest(of: final, after: "Hello world this") == "is a test of the keyboard.")
        precondition(Transcript.rest(of: final, after: "hello world, this is a test of the keyboard") == "")
        precondition(Transcript.rest(of: final, after: "Hello world thi") == "is a test of the keyboard.")
        precondition(Transcript.rest(of: final, after: "Hello word this is") == "a test of the keyboard.")
        precondition(Transcript.rest(of: "the cat saw the dog", after: "the") == "cat saw the dog")
        precondition(Transcript.rest(of: "", after: "kept") == "")
        precondition(Transcript.rest(of: "one two", after: "something else entirely here") == "")
        precondition(Transcript.rest(of: "and then I told him that I said no", after: "and then I said") == "him that I said no")
        precondition(Transcript.rest(of: "so I think it is fine and that is all", after: "so I think that") == "is fine and that is all")
        precondition(Transcript.rest(of: "I'm going to go home", after: "I'm gonna") == "to go home")
    }

    /// A host that ignores marked text never showed the draft. Counting its words as
    /// already typed would cut them off the final text.
    static func attemptedWordsAreNotKept() {
        let final = "alpha beta gamma delta"
        let empty = target(nil, after: nil, hasText: false)
        let asked = Draft(start: tap(at: empty), before: nil, spacing: "", words: "alpha beta")
        let nowhere = placement(asked, nil, after: nil, hasText: false, ownsMark: true)
        precondition(nowhere == nil)
        precondition(Transcript.rest(of: final, after: asked.kept(nowhere, retyped: false)) == final)

        // In a field that already holds text the same host looks like one holding the mark.
        let busy = Draft(start: tap(at: target("Existing text")), before: "Existing text", spacing: " ", words: "alpha beta")
        let claimed = placement(busy, "Existing text", ownsMark: true)
        precondition(claimed == .marked && busy.shown == nil)
        precondition(busy.kept(nil, retyped: false) == "", "A mark the host never answered for is not text in the field")
        precondition(busy.kept(claimed, retyped: true) == "alpha beta", "Words the keyboard typed in as plain text are")

        var shown = busy
        shown.shown = true
        precondition(Transcript.rest(of: final, after: shown.kept(nil, retyped: false)) == "gamma delta")
        precondition(busy.kept(.committed, retyped: false) == "alpha beta")
    }

    /// A host reports the last sentence or two before the caret, never the whole field.
    static func hostsThatReportOnlyTheEnd() {
        let words = "Alpha beta gamma. Delta epsilon zeta eta theta. Phi chi psi omega and the tail."
        let draft = Draft(start: tap(at: target("Existing")), before: "Existing", spacing: " ", words: words)
        precondition(placement(draft, "Phi chi psi omega and the tail.", ownsMark: false) == .committed)
        precondition(placement(draft, "Existing" + draft.text, ownsMark: false) == .committed)
        precondition(placement(draft, "Edited" + draft.text, ownsMark: false) == .committed, "Text before the draft may change")
        precondition(placement(draft, "tail.", ownsMark: false) == nil)
        precondition(placement(draft, "Something else entirely.", ownsMark: false) == nil)

        // iOS gives a field a new identifier each time the keyboard comes back to it. The
        // take's own words, left there when the keyboard went away, still say which field it is.
        let tail = "Phi chi psi omega and the tail."
        precondition(placement(draft, tail, field: other, ownsMark: false, mayHaveMoved: true) == .committed)
        precondition(placement(draft, tail, field: other, ownsMark: false) == nil, "Unless another take has words in that field")
        precondition(placement(draft, tail, after: "changed", field: other, ownsMark: false, mayHaveMoved: true) == nil)
        let short = Draft(start: tap(at: target("Existing")), before: "Existing", spacing: " ", words: "Yes.")
        precondition(placement(short, "Existing Yes.", ownsMark: false) == .committed)
        precondition(placement(short, "Existing Yes.", field: other, ownsMark: false, mayHaveMoved: true) == nil, "A few words are too little to know another field by")
        var followed = draft
        followed.field = other
        precondition(placement(followed, "Existing", field: other, ownsMark: true) == .marked, "A mark set after coming back")
        precondition(placement(followed, "Existing", ownsMark: true, mayHaveMoved: true) == nil)

        // "We" is all the host shows of a draft that ends "... in English. We". The words stay,
        // and the user is offered what the take says after them.
        var cut = Draft(start: tap(at: target("Existing")), before: "Existing", spacing: " ", words: "Hi, this is a test in English. We")
        precondition(cut.wordsLeft(at: target("We", field: other), before: "We") == nil, "Not for words the host never showed")
        cut.shown = true
        precondition(placement(cut, "We", field: other, ownsMark: false, mayHaveMoved: true) == nil)
        precondition(cut.wordsLeft(at: target("We", field: other), before: "We") == cut.words)
        precondition(cut.wordsLeft(at: target("They", field: other), before: "They") == nil)
        precondition(cut.wordsLeft(at: target("We", after: "changed", field: other), before: "We") == nil)
        precondition(cut.wordsLeft(at: target(nil, field: other), before: nil) == nil)
        precondition(Transcript.rest(of: "Hi, this is a test in English, we are done.", after: cut.words) == "are done.")
        // iOS dropped the marked words when the keyboard changed hands, and the field reads as it did at the tap.
        precondition(cut.wordsLeft(at: target("Existing", field: other), before: "Existing") == "")
        var fromEmpty = Draft(start: tap(at: target(nil, after: nil, hasText: false)), before: nil, spacing: "", words: "Hello there")
        fromEmpty.shown = true
        precondition(fromEmpty.wordsLeft(at: target(nil, after: nil, field: other), before: nil) == nil, "An empty field is like every other empty field")

        // A second take that landed in another field keeps the first take from being found there.
        let elsewhere = tap(at: target(nil, field: other))
        var ledger = Ledger()
        ledger.update(draft.start!.id) { $0.draft = draft }
        precondition(ledger.isFree(other, for: draft.start!.id))
        ledger.update(elsewhere.id) {
            $0.standing = .placed
            $0.field = other
        }
        precondition(!ledger.isFree(other, for: draft.start!.id) && ledger.isFree(other, for: elsewhere.id))
        ledger.update(draft.start!.id) { $0.draft = followed }
        precondition(ledger[draft.start!.id]?.home == other)

        // An unchanged field holds the mark or nothing, even when it happens to end in the same words.
        let echo = Draft(start: tap(at: target("say hello hello")), before: "say hello hello", spacing: " ", words: "hello")
        precondition(placement(echo, "say hello hello", ownsMark: false) == nil)
        precondition(placement(echo, "say hello hello", ownsMark: true) == .marked)

        precondition(HostContext.ends("Phi chi psi omega and the tail.", with: " " + words))
        precondition(HostContext.ends("Existing " + words, with: " " + words))
        precondition(HostContext.ends("Ok.", with: " Ok."))
        precondition(!HostContext.ends("Existing", with: " " + words))
        precondition(!HostContext.ends(nil, with: words) && !HostContext.ends("", with: words))
        precondition(HostContext.ends("re\u{301}sume\u{301} sent", with: "résumé sent"), "Accents compare as the field shows them")
    }

    /// What a host that ignores marked text sends back after the final text went in as marked text.
    static func refusals() {
        let text = " Hi, this is a quick dictation test in English."
        func refused(before: String?, now: String?, after: String? = nil, nowAfter: String? = nil) -> Bool {
            HostContext.refused(text, before: before, after: after, now: now, nowAfter: nowAfter)
        }
        precondition(refused(before: "Existing text.", now: "Existing text."))
        precondition(refused(before: nil, now: nil), "An empty field that stayed empty")
        precondition(refused(before: "One. Two. Existing text.", now: "Existing text."), "The host reports less than the keyboard had guessed")
        precondition(!refused(before: "Existing text.", now: "Hi, this is a quick dictation test in English."))
        precondition(!refused(before: "Existing text.", now: "Existing text." + text))
        precondition(!refused(before: "Existing text.", now: nil), "A field the host emptied was sent, not refused")
        precondition(!refused(before: "Existing text.", now: "Existing text.", nowAfter: "Hi, this is"), "The user moved the caret in front of the take")
        precondition(!HostContext.refused(" Thanks.", before: "Thanks.", after: nil, now: "Thanks.", nowAfter: nil), "A take that repeats the sentence before it cannot be told apart, and is not typed twice")
    }

    /// One finished take used to replace the one before it, even while that one still waited for its field.
    static func everyWaitingTakeIsKept() throws {
        let now = Date()
        let first = Take(id: 1, text: "first", finished: now, start: tap(at: target("Existing text")))
        let second = Take(id: 2, text: "second", finished: now, start: tap(at: target(nil, after: nil, field: other, hasText: false)))
        var state = SessionState()
        state.keep(first, at: now)
        state.keep(second, at: now)
        let restored = try reopened(state)
        precondition(restored.takes == [first, second])
        let returned = now.addingTimeInterval(12)
        precondition(restored.takes[0].delivery(to: field, at: returned, hasDraft: true) == .verifyTarget)
        precondition(restored.takes[1].delivery(to: field, at: returned, hasDraft: true) == .waitForField)

        var later = restored
        later.keep(Take(id: 3, text: "third", finished: now.addingTimeInterval(Bridge.returnWindow), start: first.start), at: now.addingTimeInterval(Bridge.returnWindow))
        precondition(later.takes.map(\.id) == [3], "A take nothing can place any more is dropped")
        var many = SessionState()
        for id in 1...30 { many.keep(Take(id: Int64(id), text: "take", finished: now, start: first.start), at: now) }
        precondition(many.takes.count == 20 && many.takes.last?.id == 30)

        let old = try decoder.decode(SessionState.self, from: Data("""
            {"phase":"armed","take":{"id":1,"text":"old","finished":0},"since":0}
            """.utf8))
        precondition(old.takes.map(\.text) == ["old"] && old.takes[0].start == nil && old.model == nil)
        let saved = String(decoding: try encoder.encode(old), as: UTF8.self)
        precondition(saved.contains(#""takes""#) && !saved.contains(#""take""#))
    }

    static func theLedgerSettlesEachTakeOnce() throws {
        let start = tap(at: target("hello"))
        let take = Take(id: 7, text: "final dictation", finished: Date(), start: start)
        let waiting = Take(id: 8, text: "another field", finished: Date(), start: tap(at: target(nil, field: other)))
        var ledger = Ledger()
        precondition(!ledger.isSettled(take))
        ledger.update(start.id) { $0.draft = Draft(start: start, before: "hello", spacing: " ", words: "final") }
        ledger.update(waiting.start!.id) { $0.draft = Draft(start: waiting.start!, before: nil, spacing: "", words: "another") }
        precondition(ledger.entries.count == 2, "Each take keeps its own words")

        for standing in [Ledger.Standing.sent, .offered] {
            ledger.update(start.id) { $0.standing = standing }
            let next = try reopened(ledger)
            precondition(next[start.id]?.standing == standing && next[start.id]?.draft == ledger[start.id]?.draft)
            precondition(!next.isSettled(take), "A take the host has not confirmed is not done with")
        }
        for standing in [Ledger.Standing.placed, .dismissed] {
            ledger.update(start.id) { $0.standing = standing }
            let next = try reopened(ledger)
            precondition(next.isSettled(take))
        }
        precondition(!ledger.isSettled(waiting) && ledger[waiting.start!.id]?.draft?.words == "another")
        ledger.remove(start.id)
        precondition(!ledger.isSettled(take) && ledger.entries.count == 1)
    }

    /// A keyboard killed after it sent a take cannot say whether the take landed. The next
    /// one reads the field. It types nothing, and offers the words when they are not there.
    static func aTakeSentBeforeARestart() {
        let start = tap(at: target("Alpha."))
        let take = Take(id: 11, text: "Sent just before the keyboard went away.", finished: Date(), start: start)
        var ledger = Ledger()
        ledger.update(start.id) {
            $0.standing = .sent
            $0.field = field
        }
        precondition(ledger.shows(take, in: field, isOwn: true, before: "Alpha. " + take.text + " "))
        precondition(ledger.shows(take, in: other, isOwn: false, before: "Alpha. " + take.text), "The field under a new identifier")
        precondition(!ledger.shows(take, in: field, isOwn: true, before: "Alpha."), "The host dropped the insert")
        precondition(!ledger.shows(take, in: other, isOwn: false, before: nil))

        var crowded = ledger
        crowded.update(UUID()) {
            $0.standing = .placed
            $0.field = other
        }
        precondition(!crowded.shows(take, in: other, isOwn: false, before: "Alpha. " + take.text), "Another take's words prove nothing")

        // Only the words after the ones the user kept in the field were sent.
        ledger.update(start.id) { $0.edit = ManualEdit(startID: start.id, target: nil, kept: "Sent just before") }
        precondition(ledger.shows(take, in: field, isOwn: true, before: "Sent just before, typed, the keyboard went away."))
    }

    static func whatAnOlderKeyboardLeftBehind() throws {
        let start = tap(at: target("hello"))
        let draft = Draft(start: start, before: "hello", spacing: " ", words: "old words")
        let edit = ManualEdit(startID: UUID(), target: target("typed"), kept: "kept words")
        let ledger = Ledger(inserted: 5, draft: draft, edit: edit)
        precondition(ledger.isSettled(Take(id: 5, text: "typed already", finished: Date(), start: start)))
        precondition(!ledger.isSettled(Take(id: 6, text: "still waiting", finished: Date(), start: start)))
        precondition(ledger[start.id]?.draft == draft && ledger[edit.startID!]?.edit == edit)
        precondition(ledger.isSettled(Take(id: 9, text: "names no field", finished: Date())))
        let unnamed = try decoder.decode(Draft.self, from: Data(#"{"spacing":" ","words":"old"}"#.utf8))
        precondition(Ledger(draft: unnamed).entries.isEmpty)
    }
}
