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

/// The edits a tap on the typing keys makes, and what the next tap may undo.
@MainActor
final class Typist {
    private(set) var trail = TapTrail()
    private var lastSpace: (time: TimeInterval, caret: Caret)?
    private var lastLetter: Caret?
    private var suggestionSpace: Caret?
    private var lastCorrection: (caret: Caret, inserted: String, original: String)?
    struct HeldWord {
        let input: WordInput
        let taps: TapTrail
        let space: CGPoint
        let output: String
        let end: Caret
    }
    private var held: HeldWord?
    var reconsider: ((HeldWord, WordInput, TapTrail, String) -> String?)?
    var committed: ((TapTrail, String) -> Void)?
    private let suggestions: (WordInput, TapTrail) -> WordSuggestions

    init(suggestions: @escaping (WordInput, TapTrail) -> WordSuggestions) {
        self.suggestions = suggestions
    }

    func canCorrect(_ proxy: UITextDocumentProxy) -> Bool {
        lastLetter != nil && lastLetter == Caret(proxy)
    }

    private var swipeEdit: (caret: Caret, candidates: [String], prefix: String)?

    func swipeChoices(in proxy: UITextDocumentProxy) -> [String]? {
        guard let edit = swipeEdit, edit.caret == Caret(proxy) else { return nil }
        return edit.candidates
    }

    func insertSwipe(_ words: [String], uppercase: Bool, locked: Bool, in proxy: UITextDocumentProxy) {
        let choices = words.map { word -> String in
            let text = proxy.smartQuotesType == .no ? word : word.replacingOccurrences(of: "'", with: "’")
            if locked || proxy.autocapitalizationType == .allCharacters { return text.uppercased() }
            return uppercase ? text.prefix(1).uppercased() + text.dropFirst() : text
        }
        guard let word = choices.first else { return }
        forget()
        let prefix = proxy.documentContextBeforeInput?.last.map {
            $0.isWhitespace || WordInput.opensWord($0) ? "" : " "
        } ?? ""
        proxy.insertText(prefix + word)
        if let caret = Caret(proxy), caret.before.hasSuffix(prefix + word) {
            swipeEdit = (caret, choices, prefix)
        }
    }

    func forget() {
        swipeEdit = nil
        held = nil
        trail = TapTrail()
        lastSpace = nil
        lastLetter = nil
        lastCorrection = nil
        suggestionSpace = nil
    }

    func choose(_ input: WordInput, _ candidate: String, in proxy: UITextDocumentProxy) {
        if let edit = swipeEdit, edit.caret == Caret(proxy), edit.candidates.contains(candidate) {
            forget()
            replaceSuffix(input.word, with: candidate, in: proxy)
            if let caret = Caret(proxy), caret.before.hasSuffix(edit.prefix + candidate) {
                swipeEdit = (caret, [candidate], edit.prefix)
            }
            return
        }
        if trail.end == Caret(proxy) { committed?(trail, candidate) }
        forget()
        let next = proxy.documentContextAfterInput?.first
        let addSpace = next == nil || next.map { !$0.isWhitespace && !".,!?;:)]}".contains($0) } == true
        replaceSuffix(input.word, with: candidate + (addSpace ? " " : ""), in: proxy)
        if next == " " { proxy.adjustTextPosition(byCharacterOffset: 1) }
        if addSpace || next == " " { suggestionSpace = Caret(proxy) }
    }

    func type(_ output: String, at point: CGPoint?, in proxy: UITextDocumentProxy) {
        let caret = Caret(proxy)
        let priorTrail = caret != nil && caret == trail.end ? trail : TapTrail()
        let previousWord = held
        let previousSpace = lastSpace
        let followsLetter = caret != nil && caret == lastLetter
        let punctuation = output.count == 1 && ".,!?;:".contains(output)
        let removeSpace = punctuation && caret != nil && caret == suggestionSpace && caret?.before.last == " "
        forget()
        let now = ProcessInfo.processInfo.systemUptime
        if output == " ", let previousSpace, now - previousSpace.time < 0.45, caret == previousSpace.caret {
            replaceSuffix(" ", with: ". ", in: proxy)
            return
        }
        if removeSpace { proxy.deleteBackward() }
        let input = followsLetter && (output == " " || punctuation) ? WordInput(proxy) : nil
        let correction = input.flatMap { suggestions($0, priorTrail).automatic }
        var joint: String?
        if let input, let previousWord, let caret,
           previousWord.end.field == caret.field, previousWord.end.after == caret.after,
           previousWord.end.before + input.word == caret.before {
            joint = reconsider?(previousWord, input, priorTrail, correction ?? input.word)
        }
        if let input, let replacement = joint ?? correction {
            let prefix = joint == nil ? "" : (previousWord!.output + " ")
            let inserted = replacement + output
            replaceSuffix(prefix + input.word, with: inserted, in: proxy)
            if let end = Caret(proxy), end.before.hasSuffix(inserted) {
                let original = joint == nil ? input.word : previousWord!.input.word + " " + input.word
                if replacement != original { lastCorrection = (end, inserted, original) }
            }
        } else {
            proxy.insertText(output == "’" && proxy.smartQuotesType == .no ? "'" : output)
        }
        if joint == nil, let input { committed?(priorTrail, correction ?? input.word) }
        if output == " ", joint == nil, let input, let point, let end = Caret(proxy),
           end.before.hasSuffix((correction ?? input.word) + " ") {
            held = HeldWord(input: input, taps: priorTrail, space: point, output: correction ?? input.word, end: end)
        }
        if let letter = output.first, output.count == 1, WordInput.wordCharacter(letter),
           let start = caret, let end = Caret(proxy), start.field == end.field, start.after == end.after,
           end.before == start.before + output {
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
        }
    }

    func delete(in proxy: UITextDocumentProxy) {
        if let edit = swipeEdit, edit.caret == Caret(proxy) {
            forget()
            replaceSuffix(edit.prefix + edit.candidates[0], with: "", in: proxy)
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
