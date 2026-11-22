import UIKit

/// Runs against the installed simulator dictionaries and the exact keyboard word tables.
@MainActor
private final class Field: NSObject, UITextDocumentProxy {
    var documentContextBeforeInput: String?
    var documentContextAfterInput: String?
    var selectedText: String?
    var documentInputMode: UITextInputMode? { nil }
    let documentIdentifier = UUID()
    var autocorrectionType = UITextAutocorrectionType.default
    var keyboardType = UIKeyboardType.default
    var hasText: Bool { documentContextBeforeInput?.isEmpty == false || documentContextAfterInput?.isEmpty == false }
    func insertText(_ text: String) {
        documentContextBeforeInput = (documentContextBeforeInput ?? "") + text
    }

    func deleteBackward() {
        documentContextBeforeInput = documentContextBeforeInput.map { String($0.dropLast()) }
    }

    func adjustTextPosition(byCharacterOffset offset: Int) {
        let before = documentContextBeforeInput ?? ""
        let after = documentContextAfterInput ?? ""
        if offset > 0 {
            documentContextBeforeInput = before + after.prefix(offset)
            documentContextAfterInput = String(after.dropFirst(offset))
        } else {
            documentContextBeforeInput = String(before.dropLast(-offset))
            documentContextAfterInput = before.suffix(-offset) + after
        }
    }
    func setMarkedText(_ markedText: String, selectedRange: NSRange) {}
    func unmarkText() {}
}

@main
enum TypingVocabularyChecks {
    @MainActor private static func typingEdits() async {
        let field = Field()
        let strip = CompletionStrip()
        strip.configure(.bilingual)
        let layout = KeyLayout()
        let typist = Typist { strip.suggestions(for: $0, trail: $1, layout: layout) }
        func updateStrip() async {
            strip.update(for: field, trail: typist.trail, layout: layout,
                         canCorrect: typist.canCorrect(field), swipeChoices: typist.swipeChoices(in: field))
            try? await Task.sleep(for: .milliseconds(80))
        }
        func swipe(_ words: [String]) { typist.insertSwipe(words, uppercase: false, locked: false, in: field) }
        func type(_ output: String) { typist.type(output, at: nil, in: field) }
        func reset(_ before: String, _ after: String? = nil) {
            typist.forget()
            field.documentContextBeforeInput = before
            field.documentContextAfterInput = after
        }

        reset("Go ahead ")
        swipe(["with"])
        type("t")
        assert(field.documentContextBeforeInput == "Go ahead with t")

        reset("Go ahead ")
        swipe(["with", "witch"])
        typist.choose(WordInput(field)!, "witch", in: field)
        type("t")
        assert(field.documentContextBeforeInput == "Go ahead witch t", "A letter after picking a swipe starts a word")

        reset("Go ahead ")
        swipe(["with"])
        swipe(["those"])
        typist.delete(in: field)
        assert(field.documentContextBeforeInput == "Go ahead with", "Delete removes one swipe")
        assert(typist.startsSwipeWord(in: field), "A short swipe can start a word after swipe undo")
        type("t")
        assert(field.documentContextBeforeInput == "Go ahead with t", "A letter after swipe undo starts a word")

        reset("Go ahead ")
        swipe(["with"])
        swipe(["those"])
        typist.delete(in: field)
        swipe(["go"])
        assert(field.documentContextBeforeInput == "Go ahead with go")

        for mark in [".", "’", ")", "]"] {
            reset("")
            swipe(["with"])
            type(mark)
            assert(field.documentContextBeforeInput == "with" + mark)
        }

        reset("")
        swipe(["with"])
        let otherField = Field()
        otherField.documentContextBeforeInput = "with"
        typist.checkSwipePosition(in: otherField)
        type("s")
        assert(field.documentContextBeforeInput == "withs", "Changing fields clears swipe spacing")

        reset("")
        swipe(["with"])
        field.adjustTextPosition(byCharacterOffset: -1)
        typist.checkSwipePosition(in: field)
        field.adjustTextPosition(byCharacterOffset: 1)
        type("s")
        assert(field.documentContextBeforeInput == "withs", "Moving the cursor clears swipe spacing")

        reset("Try swipi", " more.")
        typist.choose(WordInput(field)!, "swiping", in: field)
        type("ö")
        typist.delete(in: field)
        for letter in "look better" {
            type(String(letter))
            await updateStrip()
            assert(!strip.shownTitles.isEmpty, "Suggestions remain visible when inserting before a word")
        }
        assert(field.documentContextBeforeInput == "Try swiping look better" && field.documentContextAfterInput == " more.")

        field.autocorrectionType = .no
        await updateStrip()
        assert(strip.shownTitles.isEmpty, "Correction-disabled fields leave the strip empty")
    }

    @MainActor private static func gapHits() {
        let keys = TypingKeys(frame: CGRect(x: 0, y: 0, width: 394, height: 210))
        keys.configure(swedish: true, needsGlobe: false)
        keys.layoutIfNeeded()
        var deletes = 0
        var output = ""
        keys.startDelete = { deletes += 1 }
        keys.insert = { value, _ in output += value }
        for point in [CGPoint(x: 344.33, y: 116.67), CGPoint(x: 343.33, y: 125),
                      CGPoint(x: 344, y: 132), CGPoint(x: 347.33, y: 144.33)] {
            keys.tap(at: point)
        }
        assert(deletes == 4 && output.isEmpty, "The four phone taps land on delete")

        let shift = keys.frame(of: "Shift")!
        let z = keys.frame(of: "z")!
        keys.tap(at: CGPoint(x: shift.maxX + (z.minX - shift.maxX) / 4, y: shift.midY))
        assert(keys.uppercase, "The gap next to shift can activate shift")
        let m = keys.frame(of: "m")!
        keys.tap(at: CGPoint(x: m.midX, y: m.midY))
        assert(output == "M" && deletes == 4, "Letter cells still type letters")

        let space = keys.frame(of: "Space")!
        let enter = keys.frame(of: "Return")!
        keys.tap(at: CGPoint(x: space.maxX - 1, y: space.midY))
        keys.tap(at: CGPoint(x: enter.minX + 1, y: enter.midY))
        assert(output == "M \n", "Touches around space and return follow their cells")
    }

    @MainActor private static func swipeWords(_ vocabulary: TypingVocabulary) {
        let layout = KeyLayout.standard(swedish: true)
        let clean = "improvements".enumerated().map { index, letter in
            SwipeSample(point: layout.centers[Lexicon.codes[letter]!]!, time: Double(index) * 0.08)
        }
        assert(vocabulary.swipe(clean, layout: layout, before: "with ").words.first?.word == "improvements")

        // The turns from the logged attempt to swipe "improvements".
        let points: [(Double, Double, Double)] = [
            (275, 14, 0), (306, 133, 0.208), (332.7, 12.7, 0.554), (229.3, 43, 0.758),
            (133.7, 23.7, 0.842), (101.7, 31.3, 0.908), (185.3, 12, 1.042), (248, 17, 1.125),
            (199.7, 85, 1.209), (200.7, 118, 1.342), (110.7, 24, 1.475), (163.7, 39, 1.575),
            (250.3, 102, 1.675), (156.3, 93.7, 1.809), (106.7, 14, 1.892), (97.3, 31.7, 1.975),
            (117.7, 53, 2.009), (202.3, 96.3, 2.105), (71.3, 57, 2.242), (192, 4.7, 2.413)
        ]
        let rough = points.map { SwipeSample(point: CGPoint(x: $0.0, y: $0.1), time: $0.2) }
        let words = vocabulary.swipe(rough, layout: layout, before: "").words
        let lexicons = ["en", "sv"].compactMap(Lexicon.init)
        assert(!words.isEmpty && words.allSatisfy { choice in
            lexicons.contains { $0.node(choice.word.lowercased()).flatMap($0.terminal) != nil }
        }, "Even the fallback only returns dictionary words")
    }

    @MainActor static func main() async {
        let vocabulary = TypingVocabulary()
        vocabulary.configure(.bilingual, dictionaries: UITextChecker.availableLanguages)
        await withCheckedContinuation { continuation in vocabulary.load { continuation.resume() } }
        let field = Field()
        precondition(SeeDocumentIdentifier(field) == field.documentIdentifier)
        precondition(SeeDocumentIdentifier(UIInputViewController().textDocumentProxy) == nil)
        func suggestions(_ text: String) -> WordSuggestions {
            field.documentContextBeforeInput = text
            return WordInput(field).map(vocabulary.suggestions) ?? WordSuggestions()
        }
        for (input, expected) in [("Teh", "The"), ("We need helpp", "help"), ("Jag skriver itne", "inte"),
                                  ("Det är mycekt", "mycket"), ("Det är braa", "bra")] {
            precondition(suggestions(input).automatic == expected, "Correction for \(input)")
        }
        for input in ["We need jag", "Vi ses tomorrow", "Send it to Jonh", "Flag XPC", "Use GitHbu"] {
            precondition(suggestions(input).automatic == nil, "Preserve \(input)")
        }
        let partial = suggestions("I will go tomo")
        precondition(partial.automatic == nil && partial.titles.contains("tomorrow"))
        precondition(suggestions("Vi ses imorg").titles.contains("imorgon"))
        precondition(suggestions("Jag ").titles.contains("har"))
        precondition(suggestions("We ").titles.contains("are"))
        for input in ["user@teh", "example.teh", "some_teh", "#teh"] {
            precondition(suggestions(input).titles.isEmpty, "Leave identifier \(input) alone")
        }
        field.selectedText = "here"
        precondition(suggestions("teh").titles.isEmpty)
        field.selectedText = nil
        field.documentContextAfterInput = "ing"
        precondition(suggestions("teh").titles.isEmpty)
        field.documentContextAfterInput = nil
        field.autocorrectionType = .no
        precondition(suggestions("teh").titles.isEmpty)
        field.autocorrectionType = .default
        field.keyboardType = .URL
        precondition(suggestions("teh").titles.isEmpty)
        field.keyboardType = .default
        vocabulary.configure(.bilingual, dictionaries: ["en_US"])
        precondition(suggestions("Vi ses imorg").titles.contains("imorgon"))
        precondition(suggestions("Jag ").titles.contains("har"))
        precondition(suggestions("Vi ses itne").automatic == "inte")
        await typingEdits()
        gapHits()
        swipeWords(vocabulary)
        print("English and Swedish typing checks passed")
    }
}
