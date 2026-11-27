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
    var autocapitalizationType = UITextAutocapitalizationType.sentences
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

        reset("")
        let neuralChoice = SwipeChoice(word: "it's", score: 3)
        typist.insertSwipe(SwipePrediction(words: [neuralChoice], previous: nil), uppercase: true, locked: false, in: field)
        assert(typist.precedingChoices(in: field).first.map { $0.geometricScore == nil } == true)

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

    @MainActor private static func swipeWords(_ vocabulary: TypingVocabulary, store: UserDefaults) {
        let layout = KeyLayout.standard(swedish: true)
        let english = Lexicon(language: "en")!
        let frames = "its".flatMap { letter in
            var frame = [Float](repeating: -.infinity, count: 65)
            frame[Lexicon.codes[letter]!] = 0
            return frame
        }
        let matches = NeuralSwipe.beam(frames, lex: english).filter { ["its", "it's"].contains($0.word) }
        assert(matches.count == 2 && matches.allSatisfy { $0.mass == 0 })
        let clean = "improvements".enumerated().map { index, letter in
            SwipeSample(point: layout.centers[Lexicon.codes[letter]!]!, time: Double(index) * 0.08)
        }
        let enabled = vocabulary.swipe(clean, layout: layout, before: "with ").words
        assert(enabled.first?.word == "improvements" && enabled.first?.geometricScore == nil)
        store.set(false, forKey: TypingLanguages.neuralSwipeKey)
        let disabled = vocabulary.swipe(clean, layout: layout, before: "with ").words
        assert(disabled.first?.word == "improvements" && disabled.allSatisfy { $0.geometricScore != nil })
        store.removeObject(forKey: TypingLanguages.neuralSwipeKey)

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

    @MainActor private static func contractions(_ vocabulary: TypingVocabulary) {
        let layout = KeyLayout.standard(swedish: true)
        let field = Field()
        let typist = Typist { vocabulary.suggestions(for: $0, trail: $1, layout: layout) }
        let cases = [
            ("", "Dont", "Don’t"), ("I ", "didnt", "didn’t"), ("It ", "doesnt", "doesn’t"),
            ("She ", "isnt", "isn’t"), ("I ", "wasnt", "wasn’t"), ("We ", "werent", "weren’t"),
            ("OK I ", "couldnt", "couldn’t"), ("I ", "wouldnt", "wouldn’t"), ("You ", "shouldnt", "shouldn’t"),
            ("She ", "hasnt", "hasn’t"), ("I ", "havent", "haven’t"), ("I ", "hadnt", "hadn’t"),
            ("Those ", "arent", "aren’t"), ("I think ", "im", "I’m"), ("", "Ive", "I’ve"),
            ("I hope ", "youre", "you’re"), ("I think ", "theyre", "they’re"),
            ("", "Thats", "That’s"), ("Tell me ", "whats", "what’s"),
            ("I ", "cant", "can’t"), ("She ", "wont", "won’t"),
            ("We could go. ", "Lets", "Let’s"), ("She ", "lets", "lets"),
            ("The dog licked ", "its", "its"), ("Get ", "well", "well"), ("They ", "were", "were"),
            ("", "Shell", "Shell"), ("", "Shed", "Shed"), ("", "Wed", "Wed"), ("", "Hed", "Hed"),
            ("It was ", "hell", "hell"), ("I was ", "ill", "ill"), ("The psyche has an ", "id", "id"),
            ("Jag är ", "hes", "hes"),
        ]
        for (prefix, raw, expected) in cases {
            for drift in [0.0, 0.25] {
                typist.forget()
                field.documentContextBeforeInput = prefix
                for letter in raw {
                    let code = Lexicon.codes[Character(letter.lowercased())]!
                    var point = layout.centers[code]!
                    point.x += layout.pitch * drift
                    point.y += layout.pitch * drift * 0.4
                    typist.type(String(letter), at: point, in: field)
                }
                typist.type(" ", at: nil, in: field)
                precondition(field.documentContextBeforeInput == prefix + expected + " ", "Commit \(prefix)\(raw) at drift \(drift)")
            }
        }
    }

    @MainActor private static func touchProfiles(_ vocabulary: TypingVocabulary, reload: () async -> TypingVocabulary) async {
        let field = Field()
        var portrait = KeyLayout.standard(swedish: true)
        portrait.space = CGRect(x: 110, y: 160, width: 174, height: 40)
        var landscape = portrait
        landscape.reach.size.width += 200
        landscape.space.size.width += 200

        func trail(_ word: String, prefix: String = "") -> TapTrail {
            field.documentContextBeforeInput = prefix + word
            return TapTrail(taps: word.map { letter in
                Tap(typed: letter, shifted: false, point: portrait.centers[Lexicon.codes[letter]!])
            }, end: Caret(field))
        }

        let left = trail("pla")
        let first = WordInput(field)!
        field.documentContextBeforeInput = "pla "
        let boundary = Caret(field)!
        let right = trail("et", prefix: "pla ")
        let second = WordInput(field)!
        let n = portrait.centers[Lexicon.codes["n"]!]!
        var edge = portrait
        edge.space.origin.y = n.y - 1
        let mistakenSpace = Typist.HeldWord(input: first, taps: left, space: n, layout: edge, output: "pla", end: boundary)
        precondition(vocabulary.reconsider(mistakenSpace, input: second, trail: right, chosen: "et", layout: edge) == "planet")
        precondition(vocabulary.offsets(for: edge)[30] == 0, "A tap reinterpreted as a letter does not train space")

        for (layout, shift) in [(portrait, -9.0), (landscape, 6.0)] {
            let point = CGPoint(x: layout.space.midX, y: layout.space.midY + shift)
            let keptSpace = Typist.HeldWord(input: first, taps: left, space: point, layout: layout, output: "pla", end: boundary)
            precondition(vocabulary.reconsider(keptSpace, input: second, trail: right, chosen: "et", layout: layout) == nil)
        }
        let up = vocabulary.offsets(for: portrait)[30]
        let down = vocabulary.offsets(for: landscape)[30]
        precondition(up < 0 && down > 0 && vocabulary.offsets(for: portrait)[30] == up)
        let reloaded = await reload()
        precondition(reloaded.offsets(for: landscape)[30] == down && reloaded.offsets(for: portrait)[30] == up)
    }

    @MainActor static func main() async {
        let suite = UUID().uuidString
        let store = UserDefaults(suiteName: suite)!
        defer { store.removePersistentDomain(forName: suite) }
        func loadedVocabulary() async -> TypingVocabulary {
            let vocabulary = TypingVocabulary(store: store)
            vocabulary.configure(.bilingual, dictionaries: UITextChecker.availableLanguages)
            await withCheckedContinuation { continuation in vocabulary.load { continuation.resume() } }
            return vocabulary
        }
        var vocabulary = await loadedVocabulary()
        let field = Field()
        precondition(SeeDocumentIdentifier(field) == field.documentIdentifier)
        precondition(SeeDocumentIdentifier(UIInputViewController().textDocumentProxy) == nil)
        func suggestions(_ text: String) -> WordSuggestions {
            field.documentContextBeforeInput = text
            return WordInput(field).map(vocabulary.suggestions) ?? WordSuggestions()
        }
        let layout = KeyLayout.standard(swedish: true)
        for (prefix, word, expected) in [("I think we ", "sho", Character("u")), ("Det är ", "sv", Character("e"))] {
            field.documentContextBeforeInput = prefix + word
            let taps = word.map { letter in
                var point = layout.centers[Lexicon.codes[letter]!]!
                point.x += layout.pitch * 0.31
                return Tap(typed: letter, shifted: false, point: point)
            }
            let prior = vocabulary.keyPrior(for: WordInput(field)!, trail: TapTrail(taps: taps, end: Caret(field)), layout: layout)!
            precondition(prior.indices.max(by: { prior[$0] < prior[$1] }) == Lexicon.codes[expected])
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
        precondition(suggestions("How are ").titles.first == "you", "The two words before pick the next one")
        precondition(suggestions("Thank you ").titles.contains("for"))
        precondition(suggestions("I think we sho").titles.contains("should"), "A completion follows the word before it")
        precondition(Set(suggestions("Recom").titles) == ["Recom", "Recommend", "Recommended"],
                     "A likelier longer word comes before a rare word of the typed length")
        precondition(suggestions("").titles.contains("I"), "An empty field offers a first word")
        precondition(suggestions("Thanks. ").titles.allSatisfy { $0.first?.isUppercase == true },
                     "A word offered at the start of a sentence has its capital")
        precondition(suggestions("We met Zorblat ").titles.count == 3, "An unknown word still leads somewhere")
        precondition(suggestions("They are in t").automatic == nil, "One letter is the start of a word, not a typo")
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
        contractions(vocabulary)
        await touchProfiles(vocabulary, reload: loadedVocabulary)
        gapHits()
        swipeWords(vocabulary, store: store)
        // A user can pick the literal spelling, undo it, or keep typing after it.
        func learnLiteral(_ word: String, after prefix: String, picked: Bool = false, chosen: String? = nil) {
            field.documentContextBeforeInput = prefix + word
            let trail = TapTrail(taps: word.map { Tap(typed: $0, shifted: false, point: nil) }, end: Caret(field))
            vocabulary.learn(WordInput(field)!, trail: trail, chosen: chosen ?? word, picked: picked, layout: .standard(swedish: true))
        }
        learnLiteral("helpp", after: "We need ", picked: true)
        learnLiteral("more", after: "We need help ")
        precondition(suggestions("We need helpp").automatic == "help")
        learnLiteral("helpp", after: "We need ", picked: true)
        learnLiteral("today", after: "We need helpp ")
        vocabulary = await loadedVocabulary()
        precondition(suggestions("We need helpp").automatic == nil)
        field.documentContextBeforeInput = "Det är br"
        field.documentContextAfterInput = "aa"
        vocabulary.observeEditInsideWord(Caret(field)!)
        field.documentContextAfterInput = nil
        learnLiteral("idag", after: "Det är braa ")
        vocabulary = await loadedVocabulary()
        precondition(suggestions("Det är braa").automatic == nil)
        learnLiteral("helpp", after: "We need ")
        learnLiteral("today", after: "We need helpp ")
        precondition(store.stringArray(forKey: "typingPickedWords") == ["braa", "helpp"])
        precondition(suggestions("We need helpp").titles.contains("help"))
        let typist = Typist { input, _ in vocabulary.suggestions(for: input) }
        typist.committed = { input, trail, chosen, picked in
            vocabulary.learn(input, trail: trail, chosen: chosen, picked: picked, layout: .standard(swedish: true))
        }
        field.documentContextBeforeInput = "We need helpp"
        typist.choose(WordInput(field)!, "help", in: field)
        precondition(field.documentContextBeforeInput == "We need help ")
        vocabulary = await loadedVocabulary()
        precondition(suggestions("We need helpp").automatic == "help")
        learnLiteral("braa", after: "Det är ", chosen: "bra")
        learnLiteral("idag", after: "Det är braa ")
        precondition(suggestions("Det är braa").automatic == nil, "An undone correction keeps the spelling")
        learnLiteral("braa", after: "Det är ", chosen: "bra")
        learnLiteral("idag", after: "Det är bra ")
        vocabulary = await loadedVocabulary()
        precondition(suggestions("Det är braa").automatic == "bra")
        print("English and Swedish typing checks passed")
    }
}
