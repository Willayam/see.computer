import UIKit

struct WordInput: Equatable {
    let field: UUID
    let word: String
    let previous: [String]
    let followsEnglishI: Bool
    let sentenceStart: Bool
    let smartQuotes: Bool

    static func opensWord(_ c: Character) -> Bool { "([{\"“‘'«".contains(c) }

    static func wordCharacter(_ c: Character) -> Bool { c.isLetter || c == "'" || c == "’" }

    static func allowsCorrection(in proxy: UITextDocumentProxy) -> Bool {
        guard proxy.autocorrectionType != .no, proxy.isSecureTextEntry != true else { return false }
        switch proxy.keyboardType {
        case .URL, .emailAddress, .numberPad, .decimalPad, .phonePad, .asciiCapableNumberPad, .numbersAndPunctuation: return false
        default: return true
        }
    }

    init?(_ proxy: UITextDocumentProxy) {
        guard Self.allowsCorrection(in: proxy), proxy.selectedText?.isEmpty != false,
              let before = proxy.documentContextBeforeInput,
              proxy.documentContextAfterInput?.first.map({ Self.wordCharacter($0) || $0.isNumber || $0 == "_" }) != true
        else { return nil }
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
        let recent = sentenceStart ? [] : (sentence?.split(whereSeparator: { !Self.wordCharacter($0) }).suffix(4) ?? [])
        followsEnglishI = recent.last == "I"
        previous = recent.map { $0.lowercased().replacingOccurrences(of: "’", with: "'") }
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

    private var pickedWords: [String]
    private var pendingWord: (word: String, field: UUID, before: String, forget: Bool)?
    private let store: UserDefaults?
    private var neuralGestures = 0
    var useCanonicalSwipePass = true
    private var residual: Double
    private var count: Double
    var spread: Double { min(0.4, max(0.1, sqrt((residual + 5) / (2 * (count + 40))))) }

    private struct TouchProfile: Codable {
        let signature: [Int]
        var bias = Array(repeating: 0.0, count: 31)
        var counts = Array(repeating: 0.0, count: 31)
    }
    private var touchProfiles: [TouchProfile]
    private var touchLayout: KeyLayout?

    init(store: UserDefaults? = nil) {
        self.store = store
        pickedWords = store?.stringArray(forKey: "typingPickedWords") ?? []
        let saved = store?.dictionary(forKey: "typingResiduals")
        residual = saved?["sum"] as? Double ?? 0
        count = saved?["count"] as? Double ?? 0
        if let data = store?.data(forKey: "typingTouchProfiles"),
           let profiles = try? PropertyListDecoder().decode([TouchProfile].self, from: data) {
            touchProfiles = profiles
        } else if let saved = store?.dictionary(forKey: "typingBoundaryTouches"),
                  let signature = saved["layout"] as? [Int],
                  let bias = saved["bias"] as? [Double], bias.count == 31,
                  let counts = saved["counts"] as? [Double], counts.count == 31 {
            touchProfiles = [TouchProfile(signature: signature, bias: bias, counts: counts)]
        } else {
            touchProfiles = []
        }
    }

    private func selectTouchProfile(for layout: KeyLayout) {
        guard layout != touchLayout else { return }
        let geometry = [layout.pitch, layout.reach.width, layout.reach.height, layout.space.minX,
                        layout.space.minY, layout.space.width, layout.space.height]
        let centers: [CGFloat] = layout.centers.flatMap { [$0?.x ?? -1, $0?.y ?? -1] }
        let signature = (geometry + centers).map { Int(($0 * 10).rounded()) }
        if let index = touchProfiles.firstIndex(where: { $0.signature == signature }) {
            let profile = touchProfiles.remove(at: index)
            touchProfiles.append(profile)
        } else {
            touchProfiles.append(TouchProfile(signature: signature))
            if touchProfiles.count > 4 { touchProfiles.removeFirst() }
        }
        touchLayout = layout
    }

    func offsets(for layout: KeyLayout) -> [Double] {
        selectTouchProfile(for: layout)
        return touchProfiles[touchProfiles.count - 1].bias
    }

    private func observeHeight(_ y: CGFloat, center: CGFloat, key: Int, layout: KeyLayout) {
        let bias = (y - center) / layout.pitch
        guard abs(bias) <= 1 else { return }
        selectTouchProfile(for: layout)
        let index = touchProfiles.count - 1
        let count = min(80, touchProfiles[index].counts[key] + 1)
        touchProfiles[index].counts[key] = count
        touchProfiles[index].bias[key] += (bias - touchProfiles[index].bias[key]) / (12 + count)
    }

    private func saveTouchProfiles() {
        guard let store else { return }
        store.set(try? PropertyListEncoder().encode(touchProfiles), forKey: "typingTouchProfiles")
    }

    func observeEditInsideWord(_ caret: Caret) {
        let left = caret.before.reversed().prefix(while: \.isLetter).reversed()
        guard let right = caret.after?.prefix(while: \.isLetter), !right.isEmpty,
              caret.before.dropLast(left.count).last.map({ $0.isWhitespace || WordInput.opensWord($0) }) ?? true,
              caret.after?.dropFirst(right.count).first.map({ !$0.isNumber && $0 != "_" && $0 != "@" }) ?? true
        else { return }
        rememberIfRetained((String(left) + right).lowercased(), field: caret.field, before: caret.before + right)
    }

    private func rememberIfRetained(_ word: String, field: UUID, before: String) {
        guard (2...36).contains(word.count), word.allSatisfy(\.isLetter),
              languages.allSatisfy({ language in
                  guard let lex = tables[language.code] else { return false }
                  return lex.node(word).flatMap(lex.terminal) == nil
              }) else { return }
        pendingWord = (word, field, before, false)
    }

    func learn(_ input: WordInput, trail: TapTrail, chosen: String, picked: Bool, layout: KeyLayout) {
        let letters = chosen.lowercased()
        let original = input.word.lowercased()
        let previousPicks = pickedWords
        // Wait for another word in the same field so backspace can undo a mistaken commit.
        if let pending = pendingWord, let end = trail.end, end.field == pending.field,
           end.before.hasPrefix(pending.before),
           end.before.dropFirst(pending.before.count).first.map({ !WordInput.wordCharacter($0) }) == true {
            pickedWords.removeAll { $0 == pending.word }
            if !pending.forget {
                pickedWords.append(pending.word)
                if pickedWords.count > 192 { pickedWords.removeFirst() }
            }
        }
        pendingWord = nil
        if letters != original {
            if picked {
                pickedWords.removeAll { $0 == original }
            } else if pickedWords.contains(original), let end = trail.end {
                pendingWord = (original, end.field, String(end.before.dropLast(input.word.count)) + chosen, true)
            }
        } else if (picked || pickedWords.contains(original)), let end = trail.end {
            rememberIfRetained(original, field: end.field, before: end.before)
        }
        if pickedWords != previousPicks { store?.set(pickedWords, forKey: "typingPickedWords") }

        guard letters.count == trail.taps.count, !letters.contains(" ") else { return }
        var learnedHeight = false
        for (tap, letter) in zip(trail.taps, letters) {
            guard let point = tap.point, let code = Lexicon.codes[letter], let center = layout.centers[code] else { continue }
            let distance = hypot(point.x - center.x, point.y - center.y) / layout.pitch
            guard distance <= 1.6 else { continue }
            if "zxcvbnm".contains(letter) {
                observeHeight(point.y, center: center.y, key: code, layout: layout)
                learnedHeight = true
            }
            residual = residual * 0.999 + distance * distance
            count = count * 0.999 + 1
        }
        if learnedHeight { saveTouchProfiles() }
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

    private func hitsOwnKeys(_ word: String, trail: TapTrail, layout: KeyLayout) -> Bool {
        guard !word.isEmpty, word.count == trail.taps.count else { return false }
        return zip(word, trail.taps).allSatisfy { letter, tap in
            guard tap.typed == letter, let point = tap.point, let code = Lexicon.codes[letter],
                  let center = layout.centers[code] else { return false }
            let distance = hypot(point.x - center.x, point.y - center.y)
            return layout.centers.allSatisfy { other in
                guard let other else { return true }
                return hypot(point.x - other.x, point.y - other.y) >= distance
            }
        }
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
        let start = ProcessInfo.processInfo.systemUptime
        lazy var decoder = SwipeDecoder(samples: samples, layout: layout)
        var nativeSwedish = [NeuralSwipe.Candidate]()
        var neural: [NeuralSwipe.Candidate] = []
        if store?.object(forKey: TypingLanguages.neuralSwipeKey) as? Bool != false,
           languages.contains(where: { $0.code == "en" }), let lex = tables["en"],
           let frames = NeuralSwipe.emissions(samples, layout: layout, canonical: false) {
            neural = NeuralSwipe.beam(frames, lex: lex)
            if languages.count == 2, let sv = tables["sv"] {
                nativeSwedish = NeuralSwipe.beam(frames, lex: sv)
            }
            if useCanonicalSwipePass, layout.centers[Lexicon.codes["å"]!] != nil,
               let mapped = NeuralSwipe.emissions(samples, layout: layout, canonical: true) {
                neural = NeuralSwipe.average(neural, NeuralSwipe.beam(mapped, lex: lex))
            }
        }
        defer {
            if !neural.isEmpty {
                neuralGestures &+= 1
                if neuralGestures % 20 == 1, store != nil {
                    Diagnostics.log("swipe-neural", ["ms": (ProcessInfo.processInfo.systemUptime - start) * 1000,
                                                     "peak_mib": NeuralSwipe.peakMiB])
                }
            }
        }
        let usual = rankedSwipe(decoder, neural: neural, nativeSwedish: nativeSwedish, before: before)
        var prediction = SwipePrediction(words: usual, previous: nil)
        let trimmed = before.last == " " ? String(before.dropLast()) : before
        guard let first = previousChoices.first, let precedingScore = first.geometricScore,
              trimmed.hasSuffix(first.word), let currentScore = usual.first?.geometricScore, canReviseSwipe(first.word) else { return prediction }
        let prefix = trimmed.dropLast(first.word.count)
        var best = precedingScore + currentScore + 0.8
        for alternative in previousChoices.dropFirst() {
            guard let precedingScore = alternative.geometricScore else { continue }
            let next = rankedSwipe(decoder, neural: neural, nativeSwedish: nativeSwedish, before: prefix + alternative.word)
            guard let score = next.first?.geometricScore else { continue }
            let joint = precedingScore + score
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
        guard let first = choices.first, let firstScore = first.geometricScore, canReviseSwipe(first.word) else { return nil }
        func likelihood(_ word: String) -> Double {
            let previous = word.lowercased().replacingOccurrences(of: "’", with: "'")
            let following = next.lowercased().replacingOccurrences(of: "’", with: "'")
            return languages.compactMap { language -> Double? in
                guard let lex = tables[language.code], let id = lex.node(following).flatMap(lex.terminal)
                else { return nil }
                return lex.logProbability(id, after: lex.node(previous).flatMap(lex.terminal))
            }.max() ?? 0
        }
        var best = firstScore + likelihood(first.word) + 0.8
        var replacement: String?
        for alternative in choices.dropFirst() {
            guard let prior = alternative.geometricScore else { continue }
            let score = prior + likelihood(alternative.word)
            if score > best { best = score; replacement = alternative.word }
        }
        return replacement
    }

    private func rankedSwipe(_ decoder: @autoclosure () -> SwipeDecoder, neural: [NeuralSwipe.Candidate], nativeSwedish: [NeuralSwipe.Candidate], before: String) -> [SwipeChoice] {
        let sentence = before.suffix(128).reversed().prefix { !$0.isNewline && !".!?".contains($0) }.reversed()
        let previous = sentence.split { !WordInput.wordCharacter($0) }.suffix(4).map {
            String($0).lowercased().replacingOccurrences(of: "’", with: "'")
        }
        let weights = languageWeights(previous)
        var scores = [String: SwipeChoice]()
        var geometric = [String: [(String, Double)]]()
        var swedishEvidence = -Double.infinity
        var english = [(String, Double)]()
        for (i, language) in languages.enumerated() {
            guard let lex = tables[language.code] else { continue }
            let prior = previous.isEmpty ? lex.sentenceStart : previous.last.flatMap(lex.node).flatMap(lex.terminal)
            if language.code == "sv" {
                swedishEvidence = nativeSwedish.map {
                    $0.score(prior: lex.logProbability($0.id, after: prior), language: weights[i],
                             frequency: lex.sourceFrequency($0.id))
                }.max() ?? -.infinity
            }
            if language.code == "en", !neural.isEmpty {
                let spellings = Set(neural.map(\.word))
                english = neural.map {
                    let ambiguous = $0.word.contains("'") && spellings.contains($0.word.replacingOccurrences(of: "'", with: ""))
                    let score = $0.score(prior: lex.logProbability($0.id, after: prior), language: weights[i],
                                         frequency: lex.sourceFrequency($0.id))
                    return (lex.displayForm($0.word), score - (ambiguous ? 0.25 : 0))
                }
            }
            if languages.count > 1 || english.isEmpty {
                geometric[language.code] = decoder().candidates(in: lex, after: prior).map { ($0.0, $0.1 + weights[i]) }
            }
        }
        func add(_ candidates: [(String, Double)], offset: Double = 0, geometric: Bool = true) {
            for (word, score) in candidates {
                let spelling = english.isEmpty ? word : word.lowercased()
                if score + offset > (scores[spelling]?.score ?? -.infinity) {
                    scores[spelling] = SwipeChoice(word: word, score: score + offset, geometricScore: geometric ? score : nil)
                }
            }
        }
        func best(_ candidates: [(String, Double)]) -> (String, Double)? {
            candidates.min { $0.1 == $1.1 ? $0.0 < $1.0 : $0.1 > $1.1 }
        }
        if let top = best(english) {
            add(english, geometric: false)
            if let swedish = geometric["sv"], let bestSwedish = best(swedish),
               let en = languages.firstIndex(where: { $0.code == "en" }),
               let sv = languages.firstIndex(where: { $0.code == "sv" }) {
                let anchor = best(geometric["en"] ?? [])
                // Calibrated on real English validation and synthetic bilingual development traces.
                let neuralFit = top.1 - 0.75 * Double(top.0.count)
                let geometricFit = (max(-60, bestSwedish.1) - weights[sv]) / Double(bestSwedish.0.count)
                let difference = max(-45, bestSwedish.1) - max(-45, anchor?.1 ?? -.infinity)
                let agreement = anchor?.0.lowercased() == top.0.lowercased()
                let margin = 2 - 0.65 * neuralFit + 0.3 * (weights[sv] - weights[en])
                    + 0.9 * geometricFit + 0.06 * difference - (agreement ? 1.7 : 0)
                let supportsSwedish = swedishEvidence >= top.1 - 1 || weights[sv] > weights[en] + 1
                let geometricLead = bestSwedish.1 - (anchor?.1 ?? -.infinity)
                let calibrated = supportsSwedish && geometricLead > 0 ? max(0.1, margin) : margin
                add(swedish, offset: top.1 + calibrated - bestSwedish.1)
            }
        } else {
            for candidates in geometric.values { add(candidates) }
            if scores.isEmpty {
                for (i, language) in languages.enumerated() {
                    guard let lex = tables[language.code] else { continue }
                    let prior = previous.isEmpty ? lex.sentenceStart : previous.last.flatMap(lex.node).flatMap(lex.terminal)
                    add(decoder().candidates(in: lex, after: prior, relaxed: true).map { ($0.0, $0.1 + weights[i]) })
                }
            }
        }
        return Array(scores.values.sorted {
            $0.score == $1.score ? $0.word < $1.word : $0.score > $1.score
        }.prefix(6))
    }

    func reconsider(_ held: Typist.HeldWord, input: WordInput, trail: TapTrail, chosen: String, layout: KeyLayout) -> String? {
        let replacement = revisedBoundary(held, input: input, trail: trail, chosen: chosen, layout: layout)
        // A joint revision may put the boundary at a different tap.
        if replacement == nil, layout.space.contains(held.space) {
            observeHeight(held.space.y, center: layout.space.midY, key: 30, layout: layout)
            saveTouchProfiles()
        }
        return replacement
    }

    private func revisedBoundary(_ held: Typist.HeldWord, input: WordInput, trail: TapTrail, chosen: String, layout: KeyLayout) -> String? {
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
        let parts = bestText.lowercased().split(separator: " ")
        for (i, word) in [first.lowercased(), second.lowercased()].enumerated() where pickedWords.contains(word) {
            guard parts.count == 2, parts[i] == word else { return nil }
        }
        // A firm space separates two independent words. Keep any centered dictionary word on its side.
        if layout.space.insetBy(dx: 0, dy: 8).contains(held.space) {
            for (i, word, taps) in [(0, first, held.taps), (1, second, trail)] {
                let lower = word.lowercased()
                if parts.count == 2 && parts[i] == lower { continue }
                if centered(lower, trail: taps, layout: layout),
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

    private static let contractions = [
        "dont": "don't", "didnt": "didn't", "doesnt": "doesn't", "isnt": "isn't", "wasnt": "wasn't",
        "werent": "weren't", "couldnt": "couldn't", "wouldnt": "wouldn't", "shouldnt": "shouldn't",
        "hasnt": "hasn't", "havent": "haven't", "hadnt": "hadn't", "arent": "aren't", "cant": "can't",
        "wont": "won't", "im": "I'm", "ive": "I've", "youre": "you're", "theyre": "they're",
        "thats": "that's", "whats": "what's", "hes": "he's", "shes": "she's", "whos": "who's",
        "heres": "here's", "theres": "there's", "wheres": "where's", "hows": "how's", "lets": "let's",
    ]
    private static let uncontractedWords: Set<String> = ["its", "well", "were", "shell", "shed", "wed", "hed", "hell", "ill", "id"]

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
        let pickedLiteral = pickedWords.contains(word) && (
            trail.taps.count != word.count || trail.taps.contains { $0.point == nil } || hitsOwnKeys(word, trail: trail, layout: layout))
        let canCorrect = !protectedWords.contains(word) && !pickedLiteral
        if canCorrect, typed.first?.isLowercase == true, typed.dropFirst().first?.isUppercase == true,
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
                    let evidence: Double
                    if input.previous.count == 1, let first = input.previous.first,
                       let id = lex.node(first).flatMap(lex.terminal) {
                        evidence = lex.logProbability(id, after: lex.sentenceStart)
                    } else {
                        evidence = context
                    }
                    for next in lex.followers(prev, limit: 3) {
                        if let id = lex.node(next).flatMap(lex.terminal) {
                            completions.append((lex.displayForm(next), evidence + lex.logProbability(id, after: prev)))
                        }
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
                    let joined = lex.text(first) + lex.text(reading.last)
                    // Swedish permits productive compounds outside the bundled word list.
                    // Require a Swedish sentence before changing letters to invent one.
                    let swedishContext = languages.count == 1 || (!input.previous.isEmpty && context > log(0.6))
                    guard joined == word || swedishContext || lex.node(joined).flatMap(lex.terminal) != nil ||
                        languageWeights([lex.text(first), lex.text(reading.last)])[index] > log(0.9) else { return nil }
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
        if canCorrect, typed.dropFirst() == typed.dropFirst().lowercased(), let form = Self.contractions[word], word != "lets" || input.sentenceStart,
           let english = languages.firstIndex(where: { $0.code == "en" }),
           input.sentenceStart || input.followsEnglishI || weight[english] >= log(0.5) {
            result.automatic = display(form)
            append(display(form))
        } else if canCorrect, typed.dropFirst() == typed.dropFirst().lowercased(),
                  !Self.uncontractedWords.contains(word), word != "lets",
                  let best, best.0.lowercased() != word,
                  best.0.lowercased().replacingOccurrences(of: "'", with: "") != word,
                  !(literal.isFinite && centered(word, trail: trail, layout: layout)),
                  input.sentenceStart || typed.first?.isUppercase != true || shiftedName {
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
            let replacement = best.0.lowercased()
            if literal.isFinite, replacement.count == word.count,
               replacement.allSatisfy(\.isLetter), zip(word, replacement).filter({ $0 != $1 }).count >= 2,
               hitsOwnKeys(word, trail: trail, layout: layout) {
                threshold += 5
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
