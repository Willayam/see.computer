import Foundation

@main
enum DraftChecks {
    static func main() throws {
        let field = UUID()
        let other = UUID()
        func target(_ before: String?, after: String? = "next", selected: String? = nil,
                    field id: UUID? = nil, hasText: Bool = true) -> DictationTarget {
            DictationTarget(field: id ?? field, before: before, after: after, selected: selected, hasText: hasText)
        }
        let origin = target("hello")
        let start = DictationStart(id: UUID(), asked: Date(), target: origin)
        let draft = Draft(start: start, before: "hello", spacing: " ", words: "a long preview ending in words")
        let full = "hello" + draft.text
        let committed = target(full)

        precondition(origin.matches(target("hello")))
        precondition(!origin.matches(target("hello", field: other)))
        precondition(!origin.matches(target("hello", selected: "next")))
        precondition(!origin.matches(target("hello", after: "changed")))
        precondition(draft.placement(at: origin, ownsMark: true) == .marked)
        precondition(draft.placement(at: origin, ownsMark: false) == nil)
        precondition(draft.placement(at: committed, ownsMark: false) == .committed)
        precondition(draft.placement(at: target(full, field: other), ownsMark: true) == nil)
        precondition(draft.placement(at: target(String(full.suffix(12))), ownsMark: true) == nil)
        precondition(draft.placement(at: target("edited" + draft.text), ownsMark: true) == nil)
        precondition(draft.placement(at: target(full, after: "changed"), ownsMark: true) == nil)
        precondition(draft.placement(at: target(full, selected: "next"), ownsMark: true) == nil)
        precondition(draft.placement(at: target(full + " extra"), ownsMark: true) == nil)

        let empty = target(nil, after: nil, hasText: false)
        let emptyStart = DictationStart(id: UUID(), asked: Date(), target: empty)
        let emptyDraft = Draft(start: emptyStart, before: nil, spacing: "", words: "first words")
        precondition(emptyDraft.placement(at: target(nil, after: nil), ownsMark: true) == .marked)
        precondition(emptyDraft.placement(at: target("first words", after: nil), ownsMark: false) == .committed)
        precondition(emptyDraft.placement(at: empty, ownsMark: true) == nil)

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
        let encoder = JSONEncoder()
        let decoder = JSONDecoder()
        let savedTake = try decoder.decode(Take.self, from: encoder.encode(finished))
        let restoredState = SessionState(take: savedTake)
        let savedEdit = try decoder.decode(ManualEdit.self, from: encoder.encode(edit))
        precondition(restoredState.since == nil)
        precondition(savedEdit.target(for: restoredState.take!.start!, at: edited) == edited)
        precondition(savedEdit.target(for: emptyStart, at: edited) == nil)
        precondition(savedEdit.target(for: start, at: origin) == nil)
        precondition(savedEdit.target(for: start, at: target("hello typed", field: other)) == nil)
        precondition(savedEdit.target(for: start, at: target("hello typed", selected: "next")) == nil)
        precondition(ManualEdit(startID: start.id, target: nil).target(for: start, at: edited) == nil)

        let handoff = Handoff(asked: start.asked, start: start)
        let handoffData = try encoder.encode(handoff)
        let savedHandoff = try decoder.decode(Handoff.self, from: handoffData)
        precondition(savedHandoff.start?.target.matches(origin) == true)
        precondition(savedHandoff.start?.target.matches(target("hello", field: other)) == false)
        precondition(!String(decoding: handoffData, as: UTF8.self).contains("hello"))

        let oldState = try decoder.decode(SessionState.self, from: Data("""
            {"phase":"armed","take":{"id":1,"text":"old","finished":0},"since":0}
            """.utf8))
        let oldHandoff = try decoder.decode(Handoff.self, from: Data(#"{"asked":0}"#.utf8))
        let oldDraft = try decoder.decode(Draft.self, from: Data(#"{"before":"hello","spacing":" ","words":"old"}"#.utf8))
        let oldEdit = try decoder.decode(ManualEdit.self, from: Data("""
            {"since":0,"field":"\(field)","previewStillInField":true}
            """.utf8))
        precondition(oldState.take?.text == "old" && oldState.take?.start == nil)
        precondition(oldHandoff.start == nil)
        precondition(oldDraft.placement(at: target("hello old"), ownsMark: true) == nil)
        precondition(oldEdit.startID == nil && oldEdit.target == nil)
        print("Draft checks passed")
    }
}
