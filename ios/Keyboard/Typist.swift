import UIKit

/// Destructive edits require the same field, selection and surrounding text.
struct Caret: Equatable {
    let field: UUID
    let before: String
    let after: String?

    init?(_ proxy: UITextDocumentProxy) {
        guard proxy.selectedText?.isEmpty != false, let before = proxy.documentContextBeforeInput else { return nil }
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
    private let suggestions: (WordInput, TapTrail) -> WordSuggestions

    init(suggestions: @escaping (WordInput, TapTrail) -> WordSuggestions) {
        self.suggestions = suggestions
    }

    func canCorrect(_ proxy: UITextDocumentProxy) -> Bool {
        lastLetter != nil && lastLetter == Caret(proxy)
    }

    func forget() {
        trail = TapTrail()
        lastSpace = nil
        lastLetter = nil
        lastCorrection = nil
        suggestionSpace = nil
    }

    func choose(_ input: WordInput, _ candidate: String, in proxy: UITextDocumentProxy) {
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
        if followsLetter, output == " " || punctuation,
           let input = WordInput(proxy), !input.word.isEmpty, let correction = suggestions(input, priorTrail).automatic {
            let inserted = correction + output
            replaceSuffix(input.word, with: inserted, in: proxy)
            if let end = Caret(proxy), end.before.hasSuffix(inserted) {
                lastCorrection = (end, inserted, input.word)
            }
        } else {
            proxy.insertText(output == "’" && proxy.smartQuotesType == .no ? "'" : output)
        }
        if let letter = output.first, output.count == 1, WordInput.wordCharacter(letter),
           let start = caret, let end = Caret(proxy), start.field == end.field, start.after == end.after,
           end.before == start.before + output {
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
