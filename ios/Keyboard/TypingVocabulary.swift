import UIKit

private struct WordTable {
    var counts: [String: Int] = [:]
    var following: [String: [String]] = [:]
    private var prefixes: [String: [String]] = [:]
    private var total = 0

    init(language: String) {
        guard let url = Bundle.main.url(forResource: "words-\(language)", withExtension: "tsv"),
              let data = try? String(contentsOf: url, encoding: .utf8) else { return }
        for line in data.split(separator: "\n") {
            let columns = line.split(separator: "\t")
            guard columns.count >= 2, let count = Int(columns[1]) else { continue }
            let word = String(columns[0])
            counts[word] = count
            total += count
            if columns.count > 2 { following[word] = columns.dropFirst(2).map(String.init) }
            // The file is in frequency order, so each bucket is too.
            prefixes[String(word.prefix(2)), default: []].append(word)
            if word.count > 1 {
                let first = String(word.prefix(1))
                if prefixes[first, default: []].count < 8 { prefixes[first, default: []].append(word) }
            }
        }
    }

    func completions(_ prefix: String) -> [String] {
        let bucket = prefixes[String(prefix.prefix(2))] ?? []
        return Array(bucket.lazy.filter { $0 != prefix && $0.hasPrefix(prefix) }.prefix(3))
    }

    func frequency(_ word: String) -> Double {
        Double(counts[word] ?? 0) / Double(max(total, 1))
    }
}

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
    private nonisolated static let tables = ["en": WordTable(language: "en"), "sv": WordTable(language: "sv")]
    private var tables: [String: WordTable] = [:]
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

    private func orderedLanguages(for input: WordInput) -> [Language] {
        guard languages.count == 2, !input.previous.isEmpty else { return languages }
        // Recent words carry the most weight when the user changes languages mid-message.
        let evidence = input.previous.reduce(0.0) { score, word in
            let first = tables[languages[0].code]?.frequency(word) ?? 0
            let second = tables[languages[1].code]?.frequency(word) ?? 0
            return score * 0.55 + log((first + 0.00002) / (second + 0.00002))
        }
        return evidence < -0.4 ? languages.reversed() : languages
    }

    func suggestions(for input: WordInput) -> WordSuggestions {
        let ordered = orderedLanguages(for: input)
        let typed = input.word
        let word = typed.lowercased().replacingOccurrences(of: "’", with: "'")
        var result = WordSuggestions(titles: typed.isEmpty ? [] : [typed])
        func display(_ candidate: String, language: String) -> String {
            var value = candidate
            if typed.first?.isUppercase == true || (language == "en" && (value == "i" || value.hasPrefix("i'"))) {
                value = value.prefix(1).uppercased() + value.dropFirst()
            }
            return input.smartQuotes ? value.replacingOccurrences(of: "'", with: "’") : value
        }
        func append(_ candidate: String) {
            if result.titles.count < 3, !result.titles.contains(candidate) { result.titles.append(candidate) }
        }
        if word.isEmpty {
            guard let previous = input.previous.last else { return result }
            for language in ordered {
                for candidate in tables[language.code]?.following[previous] ?? [] {
                    append(display(candidate, language: language.code))
                }
            }
            return result
        }
        if let expansion = shortcuts[word] {
            result.automatic = expansion
            append(expansion)
            return result
        }

        let range = NSRange(word.startIndex..., in: word)
        let misspelled = word.count >= 3 && !protectedWords.contains(word) && ordered.allSatisfy { language in
            guard let dictionary = language.dictionary else { return false }
            return checker.rangeOfMisspelledWord(in: word, range: range, startingAt: 0, wrap: false, language: dictionary).location != NSNotFound
        }
        var guesses: [String: Double] = [:]
        if misspelled {
            let repeated = Self.removingRepeatedLetters(in: word)
            for (index, language) in ordered.enumerated() {
                guard let dictionary = language.dictionary else { continue }
                let table = tables[language.code]
                let spellings = checker.guesses(forWordRange: range, in: word, language: dictionary) ?? []
                let repeatedCandidates = repeated.filter { table?.counts[$0] != nil }
                let list = repeatedCandidates + spellings.prefix(6)
                for (rank, candidate) in list.enumerated() where candidate.allSatisfy(WordInput.wordCharacter) {
                    let lower = candidate.lowercased()
                    let frequency = (table?.frequency(lower) ?? 0) * 100_000
                    let contextBonus = input.previous.last.flatMap { table?.following[$0] }?.contains(lower) == true ? 1.5 : 0
                    let distanceBonus: Double
                    if repeatedCandidates.contains(lower) {
                        distanceBonus = 4
                    } else {
                        distanceBonus = Self.transposed(word, lower) ? 2.5 : 0
                    }
                    let score = log1p(frequency) + contextBonus + distanceBonus - Double(rank) * 0.35 - Double(index) * 1.2
                    let title = display(candidate, language: language.code)
                    guesses[title] = max(guesses[title] ?? -.infinity, score)
                }
            }
        }
        let ranked = guesses.sorted { $0.value == $1.value ? $0.key < $1.key : $0.value > $1.value }
        if let best = ranked.first, best.value >= 2.5, best.value - (ranked.dropFirst().first?.value ?? -.infinity) >= 0.85,
           (typed == typed.lowercased() || (input.sentenceStart && typed.dropFirst() == typed.dropFirst().lowercased())),
           Self.closeSpelling(word, best.key.lowercased().replacingOccurrences(of: "’", with: "'")) {
            result.automatic = best.key
            append(best.key)
        }
        for language in ordered {
            let table = tables[language.code]
            let predicted = input.previous.last.flatMap { table?.following[$0] } ?? []
            for candidate in predicted where candidate != word && candidate.hasPrefix(word) { append(display(candidate, language: language.code)) }
            for candidate in table?.completions(word) ?? [] { append(display(candidate, language: language.code)) }
            if result.titles.count == 3 { break }
        }
        for guess in ranked { append(guess.key) }
        return result
    }

    private static func transposed(_ a: String, _ b: String) -> Bool {
        guard a.count == b.count else { return false }
        let pairs = Array(zip(a, b))
        guard let i = pairs.firstIndex(where: { $0.0 != $0.1 }), i + 1 < pairs.count else { return false }
        return pairs[i].0 == pairs[i + 1].1 && pairs[i].1 == pairs[i + 1].0 &&
            pairs.dropFirst(i + 2).allSatisfy { $0.0 == $0.1 }
    }

    private static func removingRepeatedLetters(in word: String) -> [String] {
        var words: [String] = []
        var previous: Character?
        for index in word.indices {
            if word[index] == previous { words.append(String(word[..<index] + word[word.index(after: index)...])) }
            previous = word[index]
        }
        return words
    }

    private static func closeSpelling(_ a: String, _ b: String) -> Bool {
        if transposed(a, b) || removingRepeatedLetters(in: a).contains(b) { return true }
        // Shortening a partial word such as "tomo" to "too" rarely helps.
        if a.count <= 4 && b.count < a.count { return false }
        let limit = a.count >= 7 ? 2 : 1
        guard abs(a.count - b.count) <= limit else { return false }
        let target = Array(b)
        var row = Array(0...target.count)
        for (i, letter) in a.enumerated() {
            var diagonal = row[0]
            row[0] = i + 1
            for j in target.indices {
                let above = row[j + 1]
                row[j + 1] = min(above + 1, row[j] + 1, diagonal + (letter == target[j] ? 0 : 1))
                diagonal = above
            }
        }
        return row[target.count] <= limit
    }
}
