import UIKit

struct WordInput: Equatable {
    let field: UUID
    let word: String
    let previous: [String]
    let sentenceStart: Bool
    let smartQuotes: Bool

    static func wordCharacter(_ c: Character) -> Bool { c.isLetter || c == "'" || c == "’" }

    init?(_ proxy: UITextDocumentProxy) {
        guard proxy.autocorrectionType != .no, proxy.isSecureTextEntry != true,
              proxy.selectedText?.isEmpty != false, let before = proxy.documentContextBeforeInput,
              proxy.documentContextAfterInput?.first.map({ Self.wordCharacter($0) || $0.isNumber || $0 == "_" }) != true
        else { return nil }
        switch proxy.keyboardType {
        case .URL, .emailAddress, .numberPad, .decimalPad, .phonePad, .asciiCapableNumberPad, .numbersAndPunctuation: return nil
        default: break
        }
        let token = before.suffix(128).reversed().prefix(while: Self.wordCharacter)
        word = String(token.reversed()).trimmingCharacters(in: CharacterSet(charactersIn: "'’"))
        let leading = before.dropLast(word.count)
        if word.isEmpty {
            guard before.last?.isWhitespace == true else { return nil }
        } else {
            guard word.count <= 36, before.hasSuffix(word),
                  leading.last.map({ $0.isWhitespace || "(\"“‘'[«".contains($0) }) ?? true else { return nil }
        }
        let prior = leading.trimmingCharacters(in: .whitespaces)
        sentenceStart = prior.isEmpty || prior.last?.isNewline == true || prior.last.map { ".!?".contains($0) } == true
        let sentence = prior.suffix(128).split(whereSeparator: { $0.isNewline || ".!?".contains($0) }).last
        previous = sentenceStart ? [] : (sentence?.split(whereSeparator: { !Self.wordCharacter($0) }).suffix(4).map {
            $0.lowercased().replacingOccurrences(of: "’", with: "'")
        } ?? [])
        guard let identifier = SeeDocumentIdentifier(proxy) else { return nil }
        field = identifier
        smartQuotes = proxy.smartQuotesType != .no
    }
}

struct WordSuggestions {
    var titles: [String] = []
    var automatic: String?
}

@MainActor
final class TypingVocabulary {
    private nonisolated static let tables = ["en": Lexicon(language: "en"), "sv": Lexicon(language: "sv")].compactMapValues { $0 }
    private var tables: [String: Lexicon] = [:]
    private let checker = UITextChecker()
    private typealias Language = (code: String, dictionary: String?)
    private var languages: [Language] = []
    private var shortcuts: [String: String] = [:]
    private var protectedWords = Set<String>()

    func load(completion: @escaping () -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let loaded = Self.tables
            DispatchQueue.main.async {
                self.tables = loaded
                completion()
            }
        }
    }

    func configure(_ preference: TypingLanguages, dictionaries: [String]) {
        let preferred = Locale.preferredLanguages.map { Locale(identifier: $0) }
        let swedish = preference.usesSwedishKeys
        var codes = swedish && preference != .swedish ? ["en", "sv"] : [swedish ? "sv" : "en"]
        let firstPreferred = preferred.compactMap { $0.language.languageCode?.identifier }.first(where: codes.contains)
        if firstPreferred == "sv" { codes.reverse() }
        languages = codes.map { code in
            let region = preferred.first { $0.language.languageCode?.identifier == code }?.region?.identifier
            let regional = code + "_" + (region ?? (code == "en" ? "US" : "SE"))
            let dictionary = dictionaries.first(where: { $0 == regional }) ??
                dictionaries.first(where: { Locale(identifier: $0).language.languageCode?.identifier == code })
            return (code, dictionary)
        }
    }

    func use(_ lexicon: UILexicon) {
        shortcuts.removeAll()
        protectedWords.removeAll()
        for entry in lexicon.entries {
            let key = entry.userInput.lowercased()
            protectedWords.insert(entry.documentText.lowercased())
            if !key.isEmpty, key != entry.documentText.lowercased(), !entry.documentText.isEmpty {
                shortcuts[key] = entry.documentText
            }
        }
    }

    func suggestions(for input: WordInput) -> WordSuggestions {
        suggestions(for: input, trail: TapTrail(), layout: KeyLayout())
    }

    func suggestions(for input: WordInput, trail: TapTrail, layout: KeyLayout) -> WordSuggestions {
        let typed = input.word
        let word = typed.lowercased().replacingOccurrences(of: "’", with: "'")
        var result = WordSuggestions(titles: typed.isEmpty ? [] : [typed])
        func display(_ candidate: String) -> String {
            var text = candidate
            if typed.first?.isUppercase == true || candidate == "i" || candidate.hasPrefix("i'") {
                text = candidate.prefix(1).uppercased() + candidate.dropFirst()
            }
            return input.smartQuotes ? text.replacingOccurrences(of: "'", with: "’") : text
        }
        func append(_ title: String) {
            if result.titles.count < 3 && !result.titles.contains(title) { result.titles.append(title) }
        }
        if let expansion = shortcuts[word] {
            result.automatic = expansion
            append(expansion)
            return result
        }
        let codes = word.compactMap { Lexicon.codes[$0] }
        guard codes.count == word.count else { return result }
        let suffix = Array(trail.taps.suffix(codes.count))
        let matches = word.hasSuffix(String(suffix.map { $0.typed }).replacingOccurrences(of: "’", with: "'"))
        let offset = codes.count - suffix.count
        let costs = codes.enumerated().map { i, code in
            layout.costs(letter: code, point: matches && i >= offset ? suffix[i-offset].point : nil)
        }
        let literalCost = zip(costs,codes).reduce(0.0) { $0 + $1.0[$1.1] }
        var literal = -Double.infinity, unknown = -Double.infinity
        var candidates: [(String, Double)] = [], completions: [(String, Double)] = []
        var weight = Array(repeating: 0.0, count: languages.count)
        if languages.count == 2, let first = tables[languages[0].code], let second = tables[languages[1].code] {
            func frequency(_ w: String, _ lex: Lexicon) -> Double {
                lex.node(w).flatMap(lex.terminal).map { exp(lex.unigram($0)) } ?? 0
            }
            let evidence = input.previous.reduce(0.0) { odds, previous in
                odds*0.7 + log((frequency(previous,first)+2e-6)/(frequency(previous,second)+2e-6))
            }
            let p = 1 / (1 + exp(-max(-6,min(6,evidence))))
            weight = [log(p),log1p(-p)]
        }
        for (index, language) in languages.enumerated() {
            guard let lex = tables[language.code] else { continue }
            let prev = input.sentenceStart ? lex.sentenceStart : input.previous.last.flatMap(lex.node).flatMap(lex.terminal)
            let context = weight[index]
            func score(_ id: Int) -> Double { context + lex.logProbability(id, after: prev) }
            if word.isEmpty {
                if let prev {
                    for (rank, next) in lex.followers(prev, limit: 3).enumerated() {
                        completions.append((next, context - Double(rank)*0.1))
                    }
                }
                continue
            }
            if let id = lex.node(word).flatMap(lex.terminal) { literal = max(literal, score(id)) }
            unknown = max(unknown,context + lex.unknown(word))
            var readings = TapDecoder(lexicon: lex).candidates(costs: costs)
            if suffix.allSatisfy({ $0.point == nil }) {
                // Text-only input retains the keyboard's repeated-letter and transposition corrections.
                let letters = Array(word)
                for i in letters.indices.dropFirst() {
                    var variant = letters
                    if letters[i] == letters[i-1] { variant.remove(at: i) } else { variant.swapAt(i,i-1) }
                    if let id = lex.node(String(variant)).flatMap(lex.terminal) { readings.append((id, letters[i] == letters[i-1] ? 3.8 : 5.3)) }
                }
            }
            for (id,cost) in readings { candidates.append((lex.text(id),score(id)-cost)) }
            for (rank,next) in lex.completions(word, limit: 3).enumerated() {
                completions.append((next, context - Double(rank)*0.1))
            }
        }
        candidates.sort { $0.1 == $1.1 ? $0.0 < $1.0 : $0.1 > $1.1 }
        if let best = candidates.first, best.0 != word, !protectedWords.contains(word),
           typed.dropFirst() == typed.dropFirst().lowercased(), input.sentenceStart || typed.first?.isUppercase != true {
            var threshold = literal + 1
            if !literal.isFinite {
                threshold = unknown + log(input.sentenceStart ? 0.03 : 0.01)
                let range = NSRange(word.startIndex..., in: word)
                if languages.compactMap(\.dictionary).contains(where: {
                    checker.rangeOfMisspelledWord(in: word, range: range, startingAt: 0, wrap: false, language: $0).location == NSNotFound
                }) { threshold = max(-16,threshold) }
                threshold += 2
            }
            if best.1 >= threshold - literalCost {
                result.automatic = display(best.0)
                append(display(best.0))
            }
        }
        for next in completions.sorted(by: { $0.1 > $1.1 }) { append(display(next.0)) }
        for next in candidates.prefix(3) { append(display(next.0)) }
        return result
    }
}
