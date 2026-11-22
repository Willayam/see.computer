import UIKit

private struct PhoneEvent: Decodable {
    let e: String
    let a, b, f, o, w, languages: String?
    let x, y, pitch, spread: Double?
    let keys: [String: [Double]]?
    let size, space: [Double]?
    let corrects: Bool?
    let quotes: Int?
    let t: Double?
}

// The logger retains the last 80 characters before the caret.
private func visiblePrefix(_ prefix: String, in after: String) -> String? {
    if after.hasPrefix(prefix) { return prefix }
    guard after.count == 80 else { return nil }
    var tail = prefix.dropFirst()
    while !tail.isEmpty {
        if after.hasPrefix(tail) { return String(tail) }
        tail = tail.dropFirst()
    }
    return nil
}

// Update the span when a delete reopens it. Edits after its closing space
// belong to later words. Field moves and clipped host context end observation.
private func keptSpan(_ events: [PhoneEvent], index: Int, prefix: String, inserted: String) -> String {
    var anchor = prefix
    var span = inserted
    var previous = events[index].a
    for event in events.dropFirst(index + 1) {
        guard ["key", "pick", "delete"].contains(event.e), event.b == previous,
              let after = event.a, let visible = visiblePrefix(anchor, in: after) else { break }
        anchor = visible
        previous = after
        let suffix = after.dropFirst(anchor.count)
        if span.last?.isWhitespace == true && suffix.hasPrefix(span) { continue }
        span = String(suffix)
    }
    let separators = CharacterSet.whitespacesAndNewlines.union(CharacterSet(charactersIn: ".,!?;:"))
    return span.trimmingCharacters(in: separators)
}

private func matchesRecordedGeometry(_ actual: KeyLayout, _ recorded: KeyLayout) -> Bool {
    let a = actual.space, b = recorded.space
    guard abs(a.midY - b.midY) < 0.5, abs(a.midX - b.midX) < 0.5,
          abs(a.height - b.height) < 0.5, abs(a.width - b.width) < 0.5 else { return false }
    return zip(actual.centers, recorded.centers).allSatisfy { a, b in
        guard let a, let b else { return a == b }
        return hypot(a.x - b.x, a.y - b.y) < 0.5
    }
}

@MainActor
func replayPhone(_ path: String) async throws {
    let lines = try String(contentsOfFile: path, encoding: .utf8).split(whereSeparator: \.isNewline)
    let events = try lines.map { try JSONDecoder().decode(PhoneEvent.self, from: Data($0.utf8)) }
    let vocabulary = TypingVocabulary(initialSpread: events.first(where: { $0.e == "field" })?.spread)
    await withCheckedContinuation { continuation in vocabulary.load { continuation.resume() } }
    var layout = KeyLayout()
    var field = SimField()
    let keys = TypingKeys()
    var time = 0.0
    var loggedOutput = ""
    var lastPhone: String?
    var freshWord = true
    var matchingLayout = false
    var hitTested = 0
    var loggedKeys = 0
    let typist = Typist { vocabulary.suggestions(for: $0, trail: $1, layout: layout) }
    typist.editedInsideWord = vocabulary.observeEditInsideWord
    typist.committed = { vocabulary.learn($0, chosen: $1, picked: $2, layout: layout) }
    typist.reconsider = { vocabulary.reconsider($0, input: $1, trail: $2, chosen: $3, layout: layout) }
    keys.keyPrior = { _ in
        guard let input = WordInput(field), typist.trail.end == Caret(field),
              let prior = vocabulary.keyPrior(for: input, trail: typist.trail, layout: layout) else { return nil }
        return (prior, vocabulary.spread, vocabulary.offsets(for: layout))
    }
    keys.startDelete = { typist.delete(in: field) }
    keys.insert = { output, point in
        let cased = loggedOutput.first?.isUppercase == true ? output.uppercased() : output.lowercased()
        typist.type(cased, at: point, in: field, now: time)
    }
    print("event\taction\tphone\treplay\tkept\tnext")
    for (index, event) in events.enumerated() {
        if event.e == "field" {
            guard let centers = event.keys, let pitch = event.pitch, let space = event.space, space.count == 4,
                  let size = event.size, size.count == 2 else { throw CocoaError(.fileReadCorruptFile) }
            layout = KeyLayout()
            layout.pitch = pitch
            layout.space = CGRect(x: space[0], y: space[1], width: space[2], height: space[3])
            layout.reach = CGRect(x: 0, y: 0, width: size[0], height: size[1])
            for (letter, center) in centers {
                if center.count == 2, let ch = letter.first, let code = Lexicon.codes[ch] {
                    layout.centers[code] = CGPoint(x: center[0], y: center[1])
                }
            }
            var language = TypingLanguages(rawValue: event.languages ?? "") ?? .device
            if language == .device { language = centers["å"] == nil ? .english : .bilingual }
            vocabulary.configure(language, dictionaries: UITextChecker.availableLanguages)
            keys.frame = layout.reach
            for globe in [true, false] {
                keys.configure(swedish: centers["å"] != nil, needsGlobe: globe)
                keys.layoutIfNeeded()
                matchingLayout = matchesRecordedGeometry(keys.keyLayout, layout)
                if matchingLayout { break }
            }
            field = SimField()
            lastPhone = nil
            freshWord = true
            field.autocorrectionType = event.corrects == false ? .no : .default
            field.smartQuotesType = UITextSmartQuotesType(rawValue: event.quotes ?? 0) ?? .default
            typist.forget()
            continue
        }
        let before = event.b ?? ""
        if lastPhone != before || (freshWord && String(field.text.suffix(80)) != before) {
            typist.forget()
            field.text = before
        }
        lastPhone = event.a ?? ""
        freshWord = event.e != "key" || event.o?.first?.isLetter != true
        field.following = event.f
        time = (event.t ?? 0) / 1000
        var pickPreview: String?
        let original = field.text
        switch event.e {
        case "key":
            guard let output = event.o else { typist.forget(); continue }
            let point = event.x.flatMap { x in event.y.map { CGPoint(x: x, y: $0) } }
            if matchingLayout, let point, output == " " || output.first?.isLetter == true {
                hitTested += 1
                loggedOutput = output
                keys.tap(at: point)
            } else {
                loggedKeys += 1
                typist.type(output, at: point, in: field, now: time)
            }
        case "pick":
            // Show what space would have committed before the user picked from the strip.
            if let input = WordInput(field), let chosen = event.w {
                pickPreview = vocabulary.suggestions(for: input, trail: typist.trail, layout: layout).automatic ?? input.word
                typist.choose(input, chosen, in: field)
            }
        case "delete":
            typist.delete(in: field)
        default:
            typist.forget()
            field.text = event.a ?? ""
        }
        let boundary = event.e == "pick" || (event.e == "key" && event.o?.first.map { !WordInput.wordCharacter($0) } == true)
        let unfinished = event.e == "key" && (index + 1 == events.count || ["delete", "hide", "move", "field", "swipe"].contains(events[index + 1].e))
        guard boundary || unfinished, let after = event.a else { continue }
        let base = boundary ? before : after
        let prefix = String(base.dropLast(base.reversed().prefix(while: WordInput.wordCharacter).count))
        guard prefix != base else { continue }
        // A joint correction can replace the preceding word as well.
        let shared = base.commonPrefix(with: after)
        let anchor = visiblePrefix(prefix, in: after) ??
            String(shared.dropLast(shared.reversed().prefix(while: WordInput.wordCharacter).count))
        let inserted = String(after.dropFirst(anchor.count))
        let kept = keptSpan(events, index: index, prefix: anchor, inserted: inserted)
        let replayBase = boundary ? original : field.text
        let replayPrefix = replayBase.dropLast(replayBase.reversed().prefix(while: WordInput.wordCharacter).count)
        let replayShared = original.commonPrefix(with: field.text)
        let replayAnchor = field.text.hasPrefix(replayPrefix) ? replayPrefix :
            replayShared.dropLast(replayShared.reversed().prefix(while: WordInput.wordCharacter).count)
        let current = String(field.text.dropFirst(replayAnchor.count))
        let next = WordInput(field).map { vocabulary.suggestions(for: $0, trail: typist.trail, layout: layout).titles } ?? []
        let values = [index.description, event.e, inserted, pickPreview ?? current, kept, next.joined(separator: " | ")]
        print(values.map { $0.trimmingCharacters(in: .whitespacesAndNewlines).debugDescription }.joined(separator: "\t"))
    }
    let coverage = "Hit-tested \(hitTested) taps. Used the logged key for \(loggedKeys) taps.\n"
    FileHandle.standardError.write(Data(coverage.utf8))
}
