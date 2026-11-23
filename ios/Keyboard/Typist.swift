import UIKit

/// Destructive edits require the same field, selection and surrounding text.
struct Caret: Equatable {
    let field: UUID
    let before: String
    let after: String?

    init?(_ proxy: UITextDocumentProxy) {
        // Some hosts report an empty field as no context rather than as an empty string.
        guard proxy.selectedText?.isEmpty != false,
              let before = proxy.documentContextBeforeInput ?? (proxy.hasText ? nil : "") else { return nil }
        guard let identifier = SeeDocumentIdentifier(proxy) else { return nil }
        field = identifier
        self.before = before
        after = proxy.documentContextAfterInput
    }
}

struct Tap: Equatable {
    let typed: Character
    let shifted: Bool
    let point: CGPoint?
}

struct TapTrail: Equatable {
    var taps: [Tap] = []
    var end: Caret?
}

struct SwipeChoice {
    let word: String
    let score: Double
    var geometricScore: Double? = nil
}

struct SwipePrediction {
    let words: [SwipeChoice]
    let previous: String?
}

extension SwipePrediction {
    init(_ words: [String]) {
        self.init(words: words.map { SwipeChoice(word: $0, score: 0, geometricScore: 0) }, previous: nil)
    }
}

/// The edits a tap on the typing keys makes, and what the next tap may undo.
@MainActor
final class Typist {
    private(set) var trail = TapTrail()
    private var lastSpace: (time: TimeInterval, caret: Caret)?
    private var lastLetter: Caret?
    private var suggestionSpace: Caret?
    private var pendingSpace: Caret?
    private var lastCorrection: (caret: Caret, inserted: String, original: String)?
    struct HeldWord {
        let input: WordInput
        let taps: TapTrail
        let space: CGPoint
        let layout: KeyLayout
        let output: String
        let end: Caret
    }
    private var held: HeldWord?
    var reconsider: ((HeldWord, WordInput, TapTrail, String) -> String?)?
    var committed: ((WordInput, TapTrail, String, Bool) -> Void)?
    var editedInsideWord: ((Caret) -> Void)?
    private let suggestions: (WordInput, TapTrail) -> WordSuggestions

    init(suggestions: @escaping (WordInput, TapTrail) -> WordSuggestions) {
        self.suggestions = suggestions
    }

    static func capitalizes(after before: String, mode: UITextAutocapitalizationType?) -> Bool {
        switch mode {
        case .allCharacters: return true
        case .words: return before.last?.isWhitespace ?? true
        case .sentences:
            let trimmed = before.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty || trimmed.last?.isNewline == true ||
                (before.last?.isWhitespace == true && trimmed.last.map { ".!?".contains($0) } == true)
        default: return false
        }
    }

    func canCorrect(_ proxy: UITextDocumentProxy) -> Bool {
        lastLetter != nil && lastLetter == Caret(proxy)
    }

    private var swipeEdit: (caret: Caret, candidates: [String], prefix: String, addedTrailingSpace: Bool)?

    private var precedingSwipe: (caret: Caret, choices: [SwipeChoice])?
    private var swipeUndo: (inserted: String, original: String)?
    var reviseSwipe: (([SwipeChoice], String) -> String?)?

    func precedingChoices(in proxy: UITextDocumentProxy) -> [SwipeChoice] {
        guard let prior = precedingSwipe, prior.caret == Caret(proxy),
              WordInput(proxy) != nil else { return [] }
        return prior.choices
    }

    func startsSwipeWord(in proxy: UITextDocumentProxy) -> Bool {
        if needsSpace(at: Caret(proxy)) { return true }
        guard let before = proxy.documentContextBeforeInput else { return !proxy.hasText }
        return Self.atWordStart(before)
    }

    private static func atWordStart(_ before: String) -> Bool {
        guard let last = before.last else { return true }
        if last.isWhitespace { return true }
        if last == "\"" { return before.dropLast().filter { $0 == "\"" }.count.isMultiple(of: 2) }
        if last == "'" { return before.dropLast().last.map { $0.isWhitespace || WordInput.opensWord($0) } ?? true }
        return WordInput.opensWord(last)
    }

    func swipeChoices(in proxy: UITextDocumentProxy) -> [String]? {
        guard let edit = swipeEdit, edit.caret == Caret(proxy) else { return nil }
        return edit.candidates
    }

    private func needsSpace(at caret: Caret?) -> Bool {
        guard let caret else { return false }
        return caret == swipeEdit?.caret || caret == pendingSpace
    }

    func insertSwipe(_ words: [String], uppercase: Bool, locked: Bool, in proxy: UITextDocumentProxy) {
        insertSwipe(SwipePrediction(words), uppercase: uppercase, locked: locked, in: proxy)
    }

    func insertSwipe(_ prediction: SwipePrediction, uppercase: Bool, locked: Bool, in proxy: UITextDocumentProxy) {
        let before = proxy.documentContextBeforeInput ?? ""
        let prefix = Self.atWordStart(before) ? "" : " "
        let capitalize = uppercase || (!prefix.isEmpty && Self.capitalizes(after: before + prefix, mode: proxy.autocapitalizationType))
        let ranked = prediction.words.map { choice -> SwipeChoice in
            let word = choice.word
            let text = proxy.smartQuotesType == .no ? word : word.replacingOccurrences(of: "'", with: "’")
            let cased = locked || proxy.autocapitalizationType == .allCharacters ? text.uppercased()
                : capitalize ? text.prefix(1).uppercased() + text.dropFirst() : text
            return SwipeChoice(word: cased, score: choice.score, geometricScore: choice.geometricScore)
        }
        let choices = ranked.prefix(3).map(\.word)
        guard let word = choices.first else { return }
        let previous = precedingChoices(in: proxy).first?.word
        forget()
        if let replacement = prediction.previous, let previous, let before = proxy.documentContextBeforeInput {
            let separator = before.last == " " ? " " : ""
            if before.hasSuffix(previous + separator) {
                let original = previous + separator
                let inserted = replacement + separator
                replaceSuffix(original, with: inserted, in: proxy)
                swipeUndo = (inserted + prefix + word, original)
            }
        }
        let trailingSpace = proxy.documentContextAfterInput?.first.map { next in
            if next == "\"" { return before.filter { $0 == "\"" }.count.isMultiple(of: 2) }
            return !next.isWhitespace && !".,!?;:…)]}’”»'".contains(next)
        } ?? false
        proxy.insertText(prefix + word + (trailingSpace ? " " : ""))
        if trailingSpace { proxy.adjustTextPosition(byCharacterOffset: -1) }
        if let caret = Caret(proxy), caret.before.hasSuffix(prefix + word) {
            swipeEdit = (caret, choices, prefix, trailingSpace)
            precedingSwipe = (caret, ranked)
        }
    }

    func checkSwipePosition(in proxy: UITextDocumentProxy) {
        let caret = Caret(proxy)
        if pendingSpace != caret { pendingSpace = nil }
        guard swipeEdit != nil || precedingSwipe != nil else { return }
        let expected = trail.end ?? swipeEdit?.caret ?? precedingSwipe?.caret
        if expected != caret {
            precedingSwipe = nil
            swipeUndo = nil
            swipeEdit = nil
        }
    }

    func forget() {
        precedingSwipe = nil
        swipeUndo = nil
        swipeEdit = nil
        held = nil
        trail = TapTrail()
        lastSpace = nil
        lastLetter = nil
        lastCorrection = nil
        suggestionSpace = nil
        pendingSpace = nil
    }

    /// The next letter starts a new word, as after a picked suggestion.
    func startNewWord(in proxy: UITextDocumentProxy) {
        forget()
        if let caret = Caret(proxy), !Self.atWordStart(caret.before) { pendingSpace = caret }
    }

    func choose(_ input: WordInput, _ candidate: String, in proxy: UITextDocumentProxy) {
        if let edit = swipeEdit, edit.caret == Caret(proxy), edit.candidates.contains(candidate) {
            let undo = swipeUndo
            forget()
            let original = edit.candidates[0]
            replaceSuffix(original, with: candidate, in: proxy)
            if let caret = Caret(proxy), caret.before.hasSuffix(edit.prefix + candidate) {
                swipeEdit = (caret, [candidate], edit.prefix, edit.addedTrailingSpace)
                if let undo, undo.inserted.hasSuffix(original) {
                    swipeUndo = (String(undo.inserted.dropLast(original.count)) + candidate, undo.original)
                }
            }
            return
        }
        let caret = Caret(proxy)
        if let caret, caret.field == input.field {
            let pickedTrail = trail.end == caret ? trail : TapTrail(end: caret)
            committed?(input, pickedTrail, candidate, true)
        }
        forget()
        let after = proxy.documentContextAfterInput
        let next = after?.first
        let addSpace = next == nil || next.map { !$0.isWhitespace && !".,!?;:)]}".contains($0) } == true
        replaceSuffix(input.word, with: candidate + (addSpace ? " " : ""), in: proxy)
        if next == " ", after?.count ?? 0 > 1 {
            // Keep the separator before the next word so new taps don't join it.
            if let caret = Caret(proxy), caret.field == input.field, caret.before.hasSuffix(candidate), caret.after == after {
                pendingSpace = caret
            }
        } else {
            if next == " " { proxy.adjustTextPosition(byCharacterOffset: 1) }
            if addSpace || next == " " { suggestionSpace = Caret(proxy) }
        }
    }

    func type(_ output: String, at point: CGPoint?, in proxy: UITextDocumentProxy, layout: KeyLayout? = nil, now: TimeInterval = ProcessInfo.processInfo.systemUptime) {
        if output.count == 1, output.first?.isLetter == true, needsSpace(at: Caret(proxy)) {
            let swipe = precedingSwipe
            proxy.insertText(" ")
            if let swipe, let end = Caret(proxy), end.field == swipe.caret.field,
               end.after == swipe.caret.after, end.before == swipe.caret.before + " " {
                precedingSwipe = (end, swipe.choices)
            }
        }
        let caret = Caret(proxy)
        let priorTrail = caret != nil && caret == trail.end ? trail : TapTrail()
        let previousWord = held
        let swipe = precedingSwipe
        let previousSpace = lastSpace
        let followsLetter = caret != nil && caret == lastLetter
        let punctuation = output.count == 1 && ".,!?;:".contains(output)
        let removeSpace = punctuation && caret != nil && caret == suggestionSpace && caret?.before.last == " "
        forget()
        if output == " ", let previousSpace, now - previousSpace.time < 0.45, caret == previousSpace.caret {
            replaceSuffix(" ", with: ". ", in: proxy)
            return
        }
        if removeSpace { proxy.deleteBackward() }
        let input = followsLetter && (output == " " || punctuation) ? WordInput(proxy) : nil
        let correction = input.flatMap { suggestions($0, priorTrail).automatic }
        var joint: String?
        if let input, let previousWord, previousWord.layout == layout, let caret,
           previousWord.end.field == caret.field, previousWord.end.after == caret.after,
           previousWord.end.before + input.word == caret.before {
            joint = reconsider?(previousWord, input, priorTrail, correction ?? input.word)
        }
        var swipePrefix: String?
        if let input, let swipe, let caret, let first = swipe.choices.first,
           swipe.caret.before.hasSuffix(first.word + " "), swipe.caret.field == caret.field,
           swipe.caret.after == caret.after, swipe.caret.before + input.word == caret.before,
           let revised = reviseSwipe?(swipe.choices, correction ?? input.word) {
            swipePrefix = first.word + " "
            joint = revised + " " + (correction ?? input.word)
        }
        if let input, let replacement = joint ?? correction {
            let prefix = swipePrefix ?? (joint == nil ? "" : previousWord!.output + " ")
            let inserted = replacement + output
            replaceSuffix(prefix + input.word, with: inserted, in: proxy)
            if let end = Caret(proxy), end.before.hasSuffix(inserted) {
                let originalPrefix = swipePrefix ?? (joint == nil ? "" : previousWord!.input.word + " ")
                let original = originalPrefix + input.word
                if replacement != original { lastCorrection = (end, inserted, original) }
            }
        } else {
            proxy.insertText(output == "’" && proxy.smartQuotesType == .no ? "'" : output)
        }
        if joint == nil, let input { committed?(input, priorTrail, correction ?? input.word, false) }
        if output == " ", joint == nil, let input, let point, let layout, let end = Caret(proxy),
           end.before.hasSuffix((correction ?? input.word) + " ") {
            held = HeldWord(input: input, taps: priorTrail, space: point, layout: layout, output: correction ?? input.word, end: end)
        }
        if let letter = output.first, output.count == 1, WordInput.wordCharacter(letter),
           let start = caret, let end = Caret(proxy), start.field == end.field, start.after == end.after,
           end.before == start.before + output {
            if end.after?.first?.isLetter == true, WordInput.allowsCorrection(in: proxy) {
                editedInsideWord?(end)
            }
            precedingSwipe = swipe
            held = previousWord
            trail = priorTrail
            trail.taps.append(Tap(typed: Character(output.lowercased()), shifted: letter.isUppercase, point: point))
            if trail.taps.count > 36 { trail.taps.removeFirst() }
            trail.end = end
        }
        if output.count == 1, output.first?.isLetter == true {
            lastLetter = Caret(proxy)
        } else if output == " ", let prior = caret?.before.last, prior.isLetter || prior.isNumber,
                  let end = Caret(proxy), end.before.last == " " {
            lastSpace = (now, end)
            if let swipe, swipe.caret == caret, end.before == swipe.caret.before + " ",
               end.after == swipe.caret.after, end.field == swipe.caret.field {
                precedingSwipe = (end, swipe.choices)
            }
        }
    }

    func delete(in proxy: UITextDocumentProxy) {
        if let edit = swipeEdit, edit.caret == Caret(proxy) {
            let undo = swipeUndo
            forget()
            if edit.addedTrailingSpace, edit.caret.after?.first == " " {
                proxy.adjustTextPosition(byCharacterOffset: 1)
                guard let moved = Caret(proxy), moved.field == edit.caret.field,
                      moved.before == edit.caret.before + " " else { return }
                proxy.deleteBackward()
            }
            let replacement = undo.flatMap { edit.caret.before.hasSuffix($0.inserted) ? $0 : nil }
            let removed = replacement?.inserted ?? edit.prefix + edit.candidates[0]
            let restored = replacement?.original ?? ""
            replaceSuffix(removed, with: restored, in: proxy)
            if let caret = Caret(proxy), caret.field == edit.caret.field,
               caret.before == edit.caret.before.dropLast(removed.count) + restored,
               !Self.atWordStart(caret.before) {
                pendingSpace = caret
            }
            return
        }
        let prior = trail
        let owned = prior.end != nil && prior.end == Caret(proxy)
        let correction = lastCorrection
        forget()
        if let correction, correction.caret == Caret(proxy) {
            replaceSuffix(correction.inserted, with: correction.original, in: proxy)
        } else {
            proxy.deleteBackward()
            if owned, prior.taps.count > 1, let end = Caret(proxy), let start = prior.end,
               start.field == end.field, start.after == end.after, start.before.dropLast() == end.before {
                trail = prior
                trail.taps.removeLast()
                trail.end = end
            }
        }
    }

    private func replaceSuffix(_ original: String, with replacement: String, in proxy: UITextDocumentProxy) {
        let shared = zip(original, replacement).prefix { $0.0 == $0.1 }.count
        for _ in original.dropFirst(shared) { proxy.deleteBackward() }
        let tail = String(replacement.dropFirst(shared))
        if !tail.isEmpty { proxy.insertText(tail) }
    }
}
