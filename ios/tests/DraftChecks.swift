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
        try partialDraftsKeepTheWholeTake()
        manualEditsKeepWholeWords()
        hostsThatReportOnlyTheEnd()
        refusals()
        try everyWaitingTakeIsKept()
        try theLedgerSettlesEachTakeOnce()
        try aSilentOrFailedInsertKeepsAllTheWords()
        try unreadableDeliveryRecordsSurviveMigration()
        try aTakeSentBeforeARestart()
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
        precondition(placement(draft, "hello", ownsMark: false) == .uncertain)
        precondition(placement(draft, full, ownsMark: false) == .committed)
        precondition(placement(draft, "hello", field: other, ownsMark: true) == nil, "A mark belongs to the field it was set in")
        precondition(placement(draft, "words", ownsMark: true) == .uncertain, "A word the field shares with the draft proves nothing")
        precondition(placement(draft, full, after: "changed", ownsMark: true) == .uncertain)
        precondition(placement(draft, full, selected: "next", ownsMark: true) == .uncertain)
        precondition(placement(draft, full + " extra", ownsMark: true) == .uncertain)

        let empty = target(nil, after: nil, hasText: false)
        let emptyStart = tap(at: empty)
        let emptyDraft = Draft(start: emptyStart, before: nil, spacing: "", words: "first words")
        precondition(placement(emptyDraft, nil, after: nil, ownsMark: true) == .marked)
        precondition(placement(emptyDraft, "first words", after: nil, ownsMark: false) == .committed)
        precondition(placement(emptyDraft, nil, after: nil, hasText: false, ownsMark: true) == .uncertain)

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
        precondition(finished.delivery(to: field, at: returned, hasDraft: false) == .offer)
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
        let direct = Handoff(asked: start.asked, host: "example.host", start: start)
        precondition(direct.returning(to: target("hello"), in: "example.host") == .start)
        precondition(direct.returning(to: target("hello", field: other), in: "example.host") == .ask)
        precondition(direct.returning(to: target("hello"), in: "another.host") == .wait)
        precondition(direct.returning(to: nil, in: "example.host") == .wait)
        precondition(direct.returning(to: target("hello"), in: "example.host",
                                      at: start.asked.addingTimeInterval(Bridge.handoffWindow)) == .expired)

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

    static func partialDraftsKeepTheWholeTake() throws {
        let prefix = "Agenda."
        let words = "Book the afternoon train to Malmö."
        let start = tap(at: target(prefix))
        let take = Take(id: 7, text: words + " Bring the tickets.", finished: Date(), start: start)
        let draft = Draft(start: start, before: prefix, spacing: " ", words: words)
        let complete = prefix + draft.text
        let responses = ["Book the afternoon", "the afternoon train", "to Malmö.", "Malmö.",
                         "Book the train to Malmö.", "unrelated text", ""]
        for shown in [true, false] {
            var old = try JSONSerialization.jsonObject(with: encoder.encode(draft)) as! [String: Any]
            old["shown"] = shown
            let restored = try decoder.decode(Draft.self, from: JSONSerialization.data(withJSONObject: old))
            for part in responses {
                let before = part.isEmpty ? prefix : prefix + " " + part
                let caret = target(before)
                precondition(restored.placement(at: caret, before: before, ownsMark: false) == .uncertain)
                let kept = restored.confirmedWords(at: caret, before: before)
                var ledger = Ledger()
                ledger.recordEdit(ManualEdit(startID: start.id, target: caret, kept: kept), for: start.id)
                let persisted = try reopened(ledger)
                precondition(kept == nil && persisted[start.id]?.standing == .unconfirmed)
                precondition(!ledger.reserveInsert(start.id, in: field, save: { _ in true }))
                precondition(persisted.offer(for: take, in: field)?.action == .copy)
                precondition(persisted.offer(for: take, in: other)?.words == take.text)
            }
        }
        precondition(draft.confirmedWords(at: target(complete), before: complete) == words)
        let edit = ManualEdit(startID: start.id, target: target(complete + " Typed."), kept: words)
        precondition(edit.remainingWords(in: take, before: complete + " Typed.", after: nil) == "Bring the tickets.")
        precondition(edit.remainingWords(in: take, before: prefix + " Book the afternoon Typed.", after: nil) == nil)
        precondition(edit.remainingWords(in: take, before: "Different prefix. " + words + " Typed.", after: nil) == nil)
        precondition(edit.remainingWords(in: take, before: nil, after: nil) == nil)
        let repeatedStart = tap(at: target("Thanks. Thanks."))
        let repeated = Take(id: 8, text: "Thanks. Goodbye.", finished: Date(), start: repeatedStart)
        let repeatEdit = ManualEdit(startID: repeatedStart.id, target: target("Thanks. Thanks. Thanks."), kept: "Thanks.")
        precondition(repeatEdit.remainingWords(in: repeated, before: "Thanks. Thanks.", after: nil) == nil)
        precondition(repeatEdit.remainingWords(in: repeated, before: "Thanks. Thanks. Thanks.", after: nil) == "Goodbye.")
        let refused = ManualEdit(startID: start.id, target: target(prefix), kept: "")
        precondition(refused.remainingWords(in: take, before: prefix, after: nil) == take.text)
        var waiting = Ledger()
        waiting.update(start.id) { $0.draft = draft }
        precondition(waiting.offer(for: take, in: other)?.action == .copy)
        precondition(waiting.offer(for: take, in: field) == nil)
        waiting.recordEdit(ManualEdit(startID: start.id, target: start.target), for: start.id)
        var sent = take
        sent.sent = true
        precondition(waiting.offer(for: sent, in: field)?.words == take.text)
        precondition(waiting.offer(for: sent, in: field)?.action == .copy)
    }

    static func manualEditsKeepWholeWords() {
        func remaining(_ kept: String, prefix: String = "Agenda.", before: String, after: String?) -> String? {
            let start = tap(at: target(prefix, after: nil))
            let take = Take(id: 9, text: kept + " Bring the tickets.", finished: Date(), start: start)
            let edit = ManualEdit(startID: start.id, target: target(before, after: after), kept: kept)
            return edit.remainingWords(in: take, before: before, after: after)
        }
        for (kept, suffix) in [("Book the afternoon", "s"), ("tåg", "et"), ("today", "’s"),
                               ("it", "'s"), ("train", "-yard"), ("name", "_2"), ("42", ".5"),
                               ("B", "2"), ("Malmo", "\u{308}"), ("👩", "\u{200D}💻")] {
            for index in suffix.indices + [suffix.endIndex] {
                let before = "Agenda. " + kept + suffix[..<index]
                precondition(remaining(kept, before: before, after: String(suffix[index...])) == nil)
            }
        }
        let kept = "Book the afternoon", rest = "Bring the tickets."
        let field = "Agenda. " + kept
        for separator in ["", ".", ", typed", " typed", "\u{a0}typed", "!” typed"] {
            precondition(remaining(kept, before: field + separator, after: nil) == rest)
            precondition(remaining(kept, before: field, after: separator) == rest)
        }
        precondition(remaining(kept, before: "Agenda." + kept, after: nil) == nil)
        precondition(remaining(kept, prefix: "Agenda. ", before: field, after: nil) == rest)
        precondition(remaining("Malmö", prefix: "“", before: "“Malmo\u{308}", after: nil) == rest)
    }

    /// A host reports the last sentence or two before the caret, never the whole field.
    static func hostsThatReportOnlyTheEnd() {
        let words = "Alpha beta gamma. Delta epsilon zeta eta theta. Phi chi psi omega and the tail."
        let draft = Draft(start: tap(at: target("Existing")), before: "Existing", spacing: " ", words: words)
        precondition(placement(draft, "Phi chi psi omega and the tail.", ownsMark: false) == .uncertain)
        precondition(placement(draft, "Existing" + draft.text, ownsMark: false) == .committed)
        precondition(placement(draft, "Edited" + draft.text, ownsMark: false) == .uncertain)
        precondition(placement(draft, "tail.", ownsMark: false) == .uncertain)
        precondition(placement(draft, "Something else entirely.", ownsMark: false) == .uncertain)

        // iOS gives a field a new identifier each time the keyboard comes back to it. The
        // take's own words, left there when the keyboard went away, still say which field it is.
        let tail = "Phi chi psi omega and the tail."
        precondition(placement(draft, "Existing" + draft.text, field: other, ownsMark: false, mayHaveMoved: true) == .committed)
        precondition(placement(draft, "Other" + draft.text, field: other, ownsMark: false, mayHaveMoved: true) == nil)
        precondition(placement(draft, tail, field: other, ownsMark: false, mayHaveMoved: true) == nil)
        precondition(!draft.canErase(before: tail))
        precondition(draft.canErase(before: "Existing" + draft.text))
        precondition(placement(draft, tail, field: other, ownsMark: false) == nil, "Unless another take has words in that field")
        precondition(placement(draft, tail, after: "changed", field: other, ownsMark: false, mayHaveMoved: true) == nil)
        let short = Draft(start: tap(at: target("Existing")), before: "Existing", spacing: " ", words: "Yes.")
        precondition(placement(short, "Existing Yes.", ownsMark: false) == .committed)
        precondition(placement(short, "Existing Yes.", field: other, ownsMark: false, mayHaveMoved: true) == nil, "A few words are too little to know another field by")
        var followed = draft
        followed.field = other
        precondition(placement(followed, "Existing", field: other, ownsMark: true) == .marked, "A mark set after coming back")
        precondition(placement(followed, "Existing", ownsMark: true, mayHaveMoved: true) == nil)

        let cut = Draft(start: tap(at: target("Existing")), before: "Existing", spacing: " ", words: "Hi, this is a test in English. We")
        precondition(placement(cut, "We", field: other, ownsMark: false, mayHaveMoved: true) == nil)

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
        precondition(placement(echo, "say hello hello", ownsMark: false) == .uncertain)
        precondition(placement(echo, "say hello hello", ownsMark: true) == .marked)

        var legacy = draft
        legacy.committedBefore = nil
        precondition(placement(legacy, "Existing" + legacy.text, field: other, ownsMark: false, mayHaveMoved: true) == .committed)
    }

    /// What a host that ignores marked text sends back after the final text went in as marked text.
    static func refusals() {
        let text = " Hi, this is a quick dictation test in English."
        func reply(_ sent: String, before: String?, now: String?, after: String? = nil,
                   nowAfter: String? = nil, hasText: Bool = true) -> HostContext.Reply {
            HostContext.reply(to: sent, before: before, after: after, now: now, nowAfter: nowAfter, hasText: hasText)
        }
        precondition(reply(text, before: "Existing text.", now: "Existing text.") == .refused)
        precondition(reply(text, before: nil, now: nil, hasText: false) == .refused)
        precondition(reply(text, before: nil, now: nil) == .uncertain)
        precondition(reply(text, before: "One. Two. Existing text.", now: "Existing text.") == .uncertain)
        precondition(reply(text, before: "Existing text.", now: "Existing text." + text) == .placed)
        precondition(reply("résumé", before: nil, now: "re\u{301}sume\u{301}") == .placed)
        precondition(reply(text, before: "Existing text.", now: nil, hasText: false) == .uncertain)
        precondition(reply(text, before: "Existing text.", now: "Existing text.", nowAfter: "Hi, this is") == .uncertain)
        precondition(reply(" Thanks.", before: "Agenda. Thanks.", now: "Agenda. Thanks.") == .uncertain)
        precondition(reply(" Thanks.", before: "Agenda. Thanks.", now: "Agenda. Thanks. Thanks.") == .placed)
        precondition(reply(" First part is missing. Only the final sentence.", before: "Agenda.", now: "Only the final sentence.") == .uncertain)
        precondition(reply(" A sentence ending in OK.", before: "Agenda. OK.", now: "OK.") == .uncertain)
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
        precondition(many.takes.map(\.id) == (1...30).map(Int64.init))

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

        for standing in [Ledger.Standing.sent, .offered, .unconfirmed] {
            ledger.update(start.id) { $0.standing = standing }
            let next = try reopened(ledger)
            precondition(next[start.id]?.standing == standing && next[start.id]?.draft == ledger[start.id]?.draft)
            precondition(!next.isSettled(take), "A take the host has not confirmed is not done with")
        }
        for standing in [Ledger.Standing.placed, .copied, .dismissed] {
            ledger.update(start.id) { $0.standing = standing }
            let next = try reopened(ledger)
            precondition(next.isSettled(take))
        }
        precondition(!ledger.isSettled(waiting) && ledger[waiting.start!.id]?.draft?.words == "another")
        ledger.remove(start.id)
        precondition(!ledger.isSettled(take) && ledger.entries.count == 1)
    }

    static func aSilentOrFailedInsertKeepsAllTheWords() throws {
        let start = tap(at: target("Hello"))
        let take = Take(id: 31, text: "Every word is still here.", finished: Date(), start: start)
        var ledger = Ledger()
        precondition(!ledger.reserveInsert(start.id, in: field, save: { _ in false }))
        precondition(ledger[start.id] == nil)
        precondition(ledger.reserveInsert(start.id, in: field, awaitingReply: true, save: { _ in true }))
        precondition(ledger.reserveInsert(start.id, in: field, from: .sent, save: { _ in true }))
        var reopened = try self.reopened(ledger)
        precondition(!reopened.isSettled(take))
        precondition(!reopened.reserveInsert(start.id, in: field, save: { _ in true }))
        let recovery = reopened.offer(for: take, in: other)
        precondition(recovery?.action == .copy && recovery?.words == take.text)
        precondition(recovery?.label == "Copy dictation")
        precondition(reopened.offer(for: take, in: field, at: take.finished.addingTimeInterval(Bridge.returnWindow)) == nil)
        reopened.update(start.id) { $0.standing = .offered }
        precondition(reopened.offer(for: take, in: other)?.label == "Insert dictation")
        precondition(reopened.offer(for: take, in: field)?.label.contains(take.text) == true)
        precondition(reopened.reserveInsert(start.id, in: other, from: .offered, save: { _ in true }))
        precondition(!reopened.reserveInsert(start.id, in: other, from: .offered, save: { _ in true }))
    }

    static func unreadableDeliveryRecordsSurviveMigration() throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        let bookmark = dir.appendingPathComponent("inserted.json")
        let file = dir.appendingPathComponent("ledger.json")
        try Data("37".utf8).write(to: bookmark)
        try Data("damaged".utf8).write(to: file)
        precondition(Ledger.read(in: dir) == nil && fm.fileExists(atPath: bookmark.path))
        try fm.removeItem(at: file)
        let migrated = Ledger.read(in: dir)
        precondition(migrated?.floor == 37 && !fm.fileExists(atPath: bookmark.path))
        precondition(Ledger.read(in: dir) == migrated)
        precondition(Ledger.read(in: nil) == nil)
    }

    static func aTakeSentBeforeARestart() throws {
        let start = tap(at: target("Alpha."))
        let take = Take(id: 11, text: "Sent just before the keyboard went away.", finished: Date(), start: start)
        var ledger = Ledger()
        ledger.update(start.id) {
            $0.standing = .sent
            $0.field = field
        }
        let contents = "Alpha. " + take.text
        let restored = try reopened(ledger)
        func result(_ before: String?, in id: UUID, after: String? = "next") -> Ledger.Standing {
            restored.resultAfterRestart(take, at: target(before, after: after, field: id), before: before)
        }
        precondition(result(contents, in: field) == .placed)
        precondition(result(contents + " ", in: field) == .placed)
        precondition(result(contents, in: field, after: "changed") == .unconfirmed)
        precondition(result("Alpha.", in: field) == .unconfirmed)
        precondition(result("keyboard went away.", in: field) == .unconfirmed)
        precondition(result(contents, in: other) == .unconfirmed)
        var returned = restored
        returned.update(start.id) { $0.standing = result("Another field.", in: other) }
        for id in [other, field] {
            let offer = returned.offer(for: take, in: id)
            precondition(offer?.action == .copy && offer?.words == take.text)
            precondition(!returned.reserveInsert(start.id, in: id, from: .offered, save: { _ in true }))
        }
        let echoed = tap(at: target("Agenda. Thanks."))
        let repeated = Take(id: 12, text: "Thanks.", finished: Date(), start: echoed)
        ledger.update(echoed.id) { $0.standing = .sent; $0.field = field }
        for (contents, expected) in [("Agenda. Thanks.", Ledger.Standing.unconfirmed), ("Agenda. Thanks. Thanks.", .placed)] {
            precondition(ledger.resultAfterRestart(repeated, at: target(contents), before: contents) == expected)
        }
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
