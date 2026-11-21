import UIKit

struct WordInput: Equatable {
    let field: UUID
    let word: String
    let previous: [String]
    let sentenceStart: Bool
    let smartQuotes: Bool

    static func opensWord(_ c: Character) -> Bool { "([{\"“‘'«".contains(c) }

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
                  leading.last.map({ $0.isWhitespace || Self.opensWord($0) }) ?? true else { return nil }
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
    private var decoders: [String: TapDecoder] = [:]
    private let checker = UITextChecker()
    private typealias Language = (code: String, dictionary: String?)
    private var languages: [Language] = []
    private var shortcuts: [String: String] = [:]
    private var protectedWords = Set<String>()

    private let store: UserDefaults?
    private var residual: Double
    private var count: Double
    var spread: Double { min(0.4, max(0.1, sqrt((residual + 5) / (2 * (count + 40))))) }

    init(store: UserDefaults? = nil) {
        self.store = store
        let saved = store?.dictionary(forKey: "typingResiduals")
        residual = saved?["sum"] as? Double ?? 0
        count = saved?["count"] as? Double ?? 0
    }

    func learn(_ trail: TapTrail, chosen: String, layout: KeyLayout) {
        let letters = chosen.lowercased()
        guard letters.count == trail.taps.count, !letters.contains(" ") else { return }
        for (tap, letter) in zip(trail.taps, letters) {
            guard let point = tap.point, let code = Lexicon.codes[letter], let center = layout.centers[code] else { continue }
            let distance = hypot(point.x - center.x, point.y - center.y) / layout.pitch
            guard distance <= 1.6 else { continue }
            residual = residual * 0.999 + distance * distance
            count = count * 0.999 + 1
        }
        store?.set(["sum": residual, "count": count], forKey: "typingResiduals")
    }

    func load(completion: @escaping () -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let loaded = Self.tables
            DispatchQueue.main.async {
                self.tables = loaded
                self.decoders = loaded.reduce(into: [:]) { decoders, entry in
                    decoders[entry.key] = TapDecoder(lexicon: entry.value, compounds: entry.key == "sv")
                }
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

    private func languageWeights(_ previousWords: [String]) -> [Double] {
        var weight = Array(repeating: 0.0, count: languages.count)
        if languages.count == 2, let first = tables[languages[0].code], let second = tables[languages[1].code] {
            func frequency(_ w: String, _ lex: Lexicon) -> Double {
                lex.node(w).flatMap(lex.terminal).map { exp(lex.unigram($0)) } ?? 0
            }
            let evidence = previousWords.reduce(0.0) { odds, previous in
                odds * 0.7 + log((frequency(previous, first) + 2e-6) / (frequency(previous, second) + 2e-6))
            }
            let p = 1 / (1 + exp(-max(-6, min(6, evidence))))
            weight = [log(p), log1p(-p)]
        }
        return weight
    }

    private func centered(_ word: String, trail: TapTrail, layout: KeyLayout) -> Bool {
        let letters = word.lowercased()
        guard !letters.isEmpty, letters.count == trail.taps.count else { return false }
        return zip(letters, trail.taps).allSatisfy { letter, tap in
            guard tap.typed == letter, let point = tap.point, let code = Lexicon.codes[letter],
                  let center = layout.centers[code] else { return false }
            return hypot(point.x - center.x, point.y - center.y) <= layout.pitch * 0.15
        }
    }

    func swipe(_ samples: [SwipeSample], layout: KeyLayout, before: String,
               previousChoices: [SwipeChoice] = []) -> SwipePrediction {
        let decoder = SwipeDecoder(samples: samples, layout: layout)
        let usual = rankedSwipe(decoder, before: before)
        var prediction = SwipePrediction(words: usual, previous: nil)
        let trimmed = before.last == " " ? String(before.dropLast()) : before
        guard let first = previousChoices.first, trimmed.hasSuffix(first.word),
              let current = usual.first, canReviseSwipe(first.word) else { return prediction }
        let prefix = trimmed.dropLast(first.word.count)
        var best = first.score + current.score + 0.8
        for alternative in previousChoices.dropFirst() {
            let next = rankedSwipe(decoder, before: prefix + alternative.word)
            guard let score = next.first?.score else { continue }
            let joint = alternative.score + score
            if joint > best {
                best = joint
                prediction = SwipePrediction(words: next, previous: alternative.word)
            }
        }
        return prediction
    }

    private func canReviseSwipe(_ word: String) -> Bool {
        !protectedWords.contains(word.lowercased()) && shortcuts[word.lowercased()] == nil
    }

    func reviseSwipe(_ choices: [SwipeChoice], followedBy next: String) -> String? {
        guard let first = choices.first, canReviseSwipe(first.word) else { return nil }
        func likelihood(_ word: String) -> Double {
            let previous = word.lowercased().replacingOccurrences(of: "’", with: "'")
            let following = next.lowercased().replacingOccurrences(of: "’", with: "'")
            return languages.compactMap { language -> Double? in
                guard let lex = tables[language.code], let id = lex.node(following).flatMap(lex.terminal)
                else { return nil }
                return lex.logProbability(id, after: lex.node(previous).flatMap(lex.terminal))
            }.max() ?? 0
        }
        var best = first.score + likelihood(first.word) + 0.8
        var replacement: String?
        for alternative in choices.dropFirst() {
            let score = alternative.score + likelihood(alternative.word)
            if score > best { best = score; replacement = alternative.word }
        }
        return replacement
    }

    private func rankedSwipe(_ decoder: SwipeDecoder, before: String) -> [SwipeChoice] {
        let sentence = before.suffix(128).reversed().prefix { !$0.isNewline && !".!?".contains($0) }.reversed()
        let previous = sentence.split { !WordInput.wordCharacter($0) }.suffix(4).map {
            String($0).lowercased().replacingOccurrences(of: "’", with: "'")
        }
        let weights = languageWeights(previous)
        var scores = [String: Double]()
        for (i, language) in languages.enumerated() {
            guard let lex = tables[language.code] else { continue }
            let prior = previous.isEmpty ? lex.sentenceStart : previous.last.flatMap(lex.node).flatMap(lex.terminal)
            for (word, score) in decoder.candidates(in: lex, after: prior) {
                scores[word] = max(scores[word] ?? -.infinity, score + weights[i])
            }
        }
        return scores.keys.sorted {
            scores[$0] == scores[$1] ? $0 < $1 : scores[$0]! > scores[$1]!
        }.prefix(3).map { SwipeChoice(word: $0, score: scores[$0]!) }
    }

    func reconsider(_ held: Typist.HeldWord, input: WordInput, trail: TapTrail, chosen: String, layout: KeyLayout) -> String? {
        let first = held.input.word, second = input.word
        guard !held.output.contains(" "), !chosen.contains(" "), second.first?.isLowercase == true,
              held.input.sentenceStart || first.first?.isLowercase == true,
              first.dropFirst() == first.dropFirst().lowercased(), second == second.lowercased(),
              ![first, second].contains(where: { protectedWords.contains($0.lowercased()) || shortcuts[$0.lowercased()] != nil }),
              held.taps.taps.count == first.count, trail.taps.count == second.count else { return nil }
        let sigma = spread * layout.pitch
        let taps = held.taps.taps + [Tap(typed: " ", shifted: false, point: held.space)] + trail.taps
        let rows = taps.compactMap { tap -> [Double]? in
            let letter = tap.typed == " " ? 0 : Lexicon.codes[tap.typed == "’" ? "'" : tap.typed]
            return letter.map { layout.costs(letter: $0, point: tap.point, spread: spread) }
        }
        guard rows.count == taps.count else { return nil }
        let spaces = taps.map { layout.spaceCost($0.point, sigma: sigma) }
        let usual = (held.output + " " + chosen).lowercased().replacingOccurrences(of: "’", with: "'")
        let weight = languageWeights(held.input.previous)
        var usualScore: Double?, best = -Double.infinity, bestText = ""
        for (i, lang) in languages.enumerated() {
            guard let lex = tables[lang.code] else { continue }
            let prev = held.input.sentenceStart ? lex.sentenceStart : held.input.previous.last.flatMap(lex.node).flatMap(lex.terminal)
            let readings = TapDecoder(lexicon: lex).candidates(costs: rows, omissions: layout.omissionCosts(spread: spread),
                textOnly: false, previous: prev, spaces: spaces, lostSpace: layout.lostSpaceCost(sigma: sigma))
            let ids = usual.split(separator: " ").map { lex.node(String($0)).flatMap(lex.terminal) }
            for (r, cost) in readings {
                let score = weight[i] + lex.logProbability(r.last, after: r.first ?? prev) - cost
                if ids.count == 2, let lead = ids[0], r.first == lead, r.last == ids[1] { usualScore = max(usualScore ?? -.infinity, score) }
                if score >= best {
                    let text = (r.first.map { lex.displayForm(lex.text($0)) + " " } ?? "") + lex.displayForm(lex.text(r.last))
                    if score > best || text < bestText { best = score; bestText = text }
                }
            }
        }
        guard best.isFinite, bestText.lowercased() != usual else { return nil }
        if let usualScore {
            guard best - usualScore >= 2 else { return nil }
        } else if bestText.contains(" ") || held.space.y >= layout.space.minY + 8 { return nil }
        // A firm space separates two independent words. Keep any centered dictionary word on its side.
        if layout.space.insetBy(dx: 0, dy: 8).contains(held.space) {
            let parts = bestText.lowercased().split(separator: " ")
            for (i, word, taps) in [(0, first, held.taps), (1, second, trail)] {
                let lower = word.lowercased()
                if (parts.count != 2 || parts[i] != lower), centered(lower, trail: taps, layout: layout),
                   languages.contains(where: { language in
                       guard let lex = tables[language.code] else { return false }
                       return lex.node(lower).flatMap(lex.terminal) != nil
                   }) {
                    return nil
                }
            }
        }
        if let split = bestText.firstIndex(of: " "), let lex = tables["sv"],
           languages.contains(where: { $0.code == "sv" }) {
            let parts = [String(bestText[..<split]).lowercased(), String(bestText[bestText.index(after: split)...]).lowercased()]
            for literal in [first.lowercased(), second.lowercased()] where parts.joined() == literal {
                if parts.allSatisfy({ lex.node($0).flatMap(lex.terminal) != nil }) { return nil }
            }
        }
        if first.first?.isUppercase == true { bestText = bestText.prefix(1).uppercased() + bestText.dropFirst() }
        return input.smartQuotes ? bestText.replacingOccurrences(of: "'", with: "’") : bestText
    }

    func keyPrior(for input: WordInput, trail: TapTrail, layout: KeyLayout) -> [Double]? {
        guard !input.word.isEmpty, !trail.taps.isEmpty,
              input.word.lowercased() == String(trail.taps.map(\.typed)),
              input.word.lowercased().allSatisfy({ Lexicon.codes[$0] != nil }), shortcuts[input.word.lowercased()] == nil
        else { return nil }
        _ = suggestions(for: input, trail: trail, layout: layout)
        var prior = Array(repeating: 0.0, count: 31)
        for (i, lang) in languages.enumerated() {
            decoders[lang.code]?.addPrior(to: &prior, weight: lastWeights[i])
        }
        let sum = prior.reduce(0, +)
        return sum > 0 ? prior.map { $0 / sum } : nil
    }

    private var lastWeights: [Double] = []

    func suggestions(for input: WordInput) -> WordSuggestions {
        suggestions(for: input, trail: TapTrail(), layout: .standard(swedish: languages.contains { $0.code == "sv" }))
    }

    func suggestions(for input: WordInput, trail: TapTrail, layout: KeyLayout) -> WordSuggestions {
        let typed = input.word
        let word = typed.lowercased().replacingOccurrences(of: "’", with: "'")
        var result = WordSuggestions(titles: typed.isEmpty ? [] : [typed])
        func display(_ candidate: String) -> String {
            var text = candidate
            if typed.first?.isUppercase == true {
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
        if !protectedWords.contains(word), typed.first?.isLowercase == true, typed.dropFirst().first?.isUppercase == true,
           typed.dropFirst(2).allSatisfy({ !$0.isUppercase }) {
            let swapped = String(typed.prefix(2).reversed()) + typed.dropFirst(2)
            if languages.contains(where: { lang in
                guard let lex = tables[lang.code] else { return false }
                return lex.node(swapped.lowercased()).flatMap(lex.terminal) != nil
            }) {
                result.automatic = swapped
                append(swapped)
            }
        }
        let codes = word.compactMap { Lexicon.codes[$0] }
        guard codes.count == word.count else { return result }
        let suffix = Array(trail.taps.suffix(codes.count))
        let matches = word.hasSuffix(String(suffix.map { $0.typed }).replacingOccurrences(of: "’", with: "'"))
        let offset = codes.count - suffix.count
        let costs = codes.enumerated().map { i, code in
            layout.costs(letter: code, point: matches && i >= offset ? suffix[i - offset].point : nil, spread: spread)
        }
        let sigma = spread * layout.pitch
        let spaces = codes.indices.map { i in
            layout.spaceCost(matches && i >= offset ? suffix[i - offset].point : nil, sigma: sigma)
        }
        let literalCost = zip(costs, codes).reduce(0.0) { $0 + $1.0[$1.1] }
        var literal = -Double.infinity, unknown = -Double.infinity
        var candidates: [(String, Double)] = [], completions: [(String, Double)] = []
        let weight = languageWeights(input.previous)
        lastWeights = weight
        for (index, language) in languages.enumerated() {
            guard let lex = tables[language.code] else { continue }
            let prev =
                input.sentenceStart ? lex.sentenceStart : input.previous.last.flatMap(lex.node).flatMap(lex.terminal)
            let context = weight[index]
            func score(_ id: Int) -> Double { context + lex.logProbability(id, after: prev) }
            if word.isEmpty {
                if let prev {
                    for (rank, next) in lex.followers(prev, limit: 3).enumerated() {
                        completions.append((lex.displayForm(next), context - Double(rank) * 0.1))
                    }
                }
                continue
            }
            if let id = lex.node(word).flatMap(lex.terminal) { literal = max(literal, score(id)) }
            unknown = max(unknown, context + lex.unknown(word))
            let readings =
                decoders[language.code]?.candidates(
                    costs: costs, omissions: layout.omissionCosts(spread: spread), textOnly: suffix.allSatisfy { $0.point == nil },
                    previous: prev, spaces: spaces, lostSpace: offset == 0 && matches ? layout.lostSpaceCost(sigma: sigma) : .infinity)
                ?? [:]
            var ranked = readings.compactMap { reading, cost -> (TapDecoder.Reading, Double)? in
                if reading.compound {
                    guard word.count >= 7, let first = reading.first,
                          lex.text(first).count >= 3, lex.text(reading.last).count >= 3 else { return nil }
                }
                if word.count >= 7, let first = reading.first, !reading.compound,
                   readings[TapDecoder.Reading(first: first, last: reading.last, compound: true)] != nil,
                   lex.text(first).count >= 3, lex.text(reading.last).count >= 3,
                   lex.text(first) + lex.text(reading.last) == word { return nil }
                return (reading, context + (reading.compound ? lex.unigram(reading.last) : lex.logProbability(reading.last, after: reading.first ?? prev)) - cost)
            }
            ranked.sort {
                if $0.1 != $1.1 { return $0.1 > $1.1 }
                if $0.0.first != $1.0.first { return ($0.0.first ?? -1) < ($1.0.first ?? -1) }
                return $0.0.last < $1.0.last
            }
            for (reading, value) in ranked.prefix(4) {
                let lead = reading.first.map { lex.displayForm(lex.text($0)) + (reading.compound ? "" : " ") } ?? ""
                candidates.append((lead + (reading.compound ? lex.text(reading.last) : lex.displayForm(lex.text(reading.last))), value))
            }
            for next in decoders[language.code]?.completions(limit: 4) ?? [] {
                let text = lex.text(next.id)
                if text.count > word.count { completions.append((lex.displayForm(text), context + next.score)) }
            }
        }
        let shiftedName = !input.sentenceStart && typed.first?.isUppercase == true &&
            matches && offset == 0 && suffix.first?.shifted == true && suffix.allSatisfy { $0.point != nil }
        candidates.sort { $0.1 == $1.1 ? $0.0 < $1.0 : $0.1 > $1.1 }
        let best = candidates.first { text, _ in
            guard shiftedName else { return true }
            guard text.first?.isUppercase == true, !text.contains(" "), text.count == codes.count else { return false }
            let letters = text.lowercased().compactMap { Lexicon.codes[$0] }
            return letters.count == costs.count && letters.enumerated().allSatisfy { index, letter in
                costs[index][letter] - costs[index][codes[index]] <= 3
            }
        }
        if let best, best.0.lowercased() != word, !protectedWords.contains(word),
            !(literal.isFinite && centered(word, trail: trail, layout: layout)),
            typed.dropFirst() == typed.dropFirst().lowercased(), input.sentenceStart || typed.first?.isUppercase != true || shiftedName
        {
            var threshold = literal + 1
            if !literal.isFinite {
                threshold = unknown + log(typed.first?.isUppercase == true ? 0.03 : 0.0002)
                let range = NSRange(word.startIndex..., in: word)
                if languages.compactMap(\.dictionary).contains(where: {
                    checker.rangeOfMisspelledWord(in: word, range: range, startingAt: 0, wrap: false, language: $0)
                        .location == NSNotFound
                }) {
                    threshold = max(-16, threshold)
                }
                threshold += 2
            }
            if best.1 >= threshold - literalCost {
                result.automatic = display(best.0)
                append(display(best.0))
            }
        }
        if let alternate = candidates.first(where: { display($0.0) != typed }) { append(display(alternate.0)) }
        for next in (completions + candidates).sorted(by: { $0.1 == $1.1 ? $0.0 < $1.0 : $0.1 > $1.1 }) {
            append(display(next.0))
        }
        return result
    }
}
