import UIKit
import simd

/// Types held-out sentences on the real keyboard views with simulated finger noise
/// and scores the words that end up in the field.
@MainActor
final class SimField: NSObject, UITextDocumentProxy {
    var text = ""
    var documentContextBeforeInput: String? { text }
    var following: String?
    var documentContextAfterInput: String? { following }
    var selectedText: String? { nil }
    var documentInputMode: UITextInputMode? { nil }
    let documentIdentifier = UUID()
    var autocorrectionType = UITextAutocorrectionType.default
    var autocapitalizationType = UITextAutocapitalizationType.sentences
    var keyboardType = UIKeyboardType.default
    var smartQuotesType = UITextSmartQuotesType.yes
    var hasText: Bool { !text.isEmpty }
    func insertText(_ inserted: String) { text += inserted }
    func deleteBackward() { if !text.isEmpty { text.removeLast() } }
    func adjustTextPosition(byCharacterOffset offset: Int) {}
    func setMarkedText(_ markedText: String, selectedRange: NSRange) {}
    func unmarkText() {}
}

/// Mirrors how KeyboardViewController wires the keys, the typist and the word model.
/// This is the one part of the simulator that changes when that wiring changes.
@MainActor
private final class Pipeline {
    let keys = TypingKeys()
    let vocabulary = TypingVocabulary()
    private(set) var field = SimField()
    private var typist: Typist!
    var swipeNanos: [UInt64] = []
    var tenLetterNanos: [UInt64] = []
    var suggestionNanos: [UInt64] = []

    init(languages: TypingLanguages, size: CGSize) async {
        keys.frame = CGRect(origin: .zero, size: size)
        keys.configure(swedish: languages.usesSwedishKeys, needsGlobe: true)
        keys.layoutIfNeeded()
        vocabulary.configure(languages, dictionaries: UITextChecker.availableLanguages)
        await withCheckedContinuation { continuation in vocabulary.load { continuation.resume() } }
        typist = Typist { [vocabulary, keys] in vocabulary.suggestions(for: $0, trail: $1, layout: keys.keyLayout) }
        keys.keyPrior = { [weak self] layout in
            guard let self, let input = WordInput(self.field), self.typist.trail.end == Caret(self.field),
                  let prior = self.vocabulary.keyPrior(for: input, trail: self.typist.trail, layout: layout)
            else { return nil }
            return (prior, self.vocabulary.spread, self.vocabulary.offsets(for: layout))
        }
        typist.reviseSwipe = vocabulary.reviseSwipe
        typist.reconsider = { [vocabulary, keys] in vocabulary.reconsider($0, input: $1, trail: $2, chosen: $3, layout: keys.keyLayout) }
        typist.editedInsideWord = vocabulary.observeEditInsideWord
        typist.committed = { [vocabulary, keys] in vocabulary.learn($0, trail: $1, chosen: $2, picked: $3, layout: keys.keyLayout) }
        keys.startDelete = { [unowned self] in self.typist.delete(in: self.field); self.keys.updateCase(for: self.field) }
        keys.insert = { [unowned self] output, point in
            self.typist.type(output, at: point, in: self.field, layout: self.keys.keyLayout)
            self.keys.updateCase(for: self.field)
        }
    }

    func configureSwipe(endpoints: Bool) {
        keys.onSwipe = { [unowned self] samples, upper, locked, shortWord in
            if shortWord != nil && !self.typist.startsSwipeWord(in: self.field) { return false }
            let start = DispatchTime.now().uptimeNanoseconds
            let layout = self.keys.keyLayout
            func nearest(_ point: CGPoint) -> Character {
                let letter = layout.centers.indices.compactMap { i in
                    layout.centers[i].map { (i, hypot(point.x - $0.x, point.y - $0.y)) }
                }.min { $0.1 < $1.1 }!.0
                return Lexicon.alphabet[letter]
            }
            let words = endpoints ? SwipePrediction([String([nearest(samples.first!.point), nearest(samples.last!.point)])]) :
                self.vocabulary.swipe(samples, layout: layout, before: self.field.text, previousChoices: self.typist.precedingChoices(in: self.field))
            self.swipeNanos.append(DispatchTime.now().uptimeNanoseconds - start)
            guard let best = words.words.first, shortWord == nil || best.word.lowercased() == shortWord else { return false }
            self.typist.insertSwipe(words, uppercase: upper, locked: locked, in: self.field)
            self.keys.updateCase(for: self.field)
            return true
        }
    }

    func checkSwipeEdits() {
        newField()
        field.insertText("(")
        typist.insertSwipe(["hello", "hallo"], uppercase: true, locked: false, in: field)
        precondition(field.text == "(Hello" && typist.swipeChoices(in: field) == ["Hello", "Hallo"])
        typist.choose(WordInput(field)!, "Hallo", in: field)
        precondition(field.text == "(Hallo")
        typist.insertSwipe(["world"], uppercase: false, locked: true, in: field)
        precondition(field.text == "(Hallo WORLD")
        typist.delete(in: field)
        precondition(field.text == "(Hallo")
        typist.insertSwipe(["again"], uppercase: false, locked: false, in: field)
        field.insertText("x")
        precondition(typist.swipeChoices(in: field) == nil)
        typist.delete(in: field)
        precondition(field.text == "(Hallo again")
        newField()
        typist.insertSwipe(["on", "in"], uppercase: false, locked: false, in: field)
        let revision = SwipePrediction(words: [SwipeChoice(word: "town", score: 0)], previous: "in")
        typist.insertSwipe(revision, uppercase: false, locked: false, in: field)
        precondition(field.text == "in town")
        typist.delete(in: field)
        precondition(field.text == "on")
        newField()
        typist.insertSwipe(["on", "in"], uppercase: false, locked: false, in: field)
        let saved = typist.reviseSwipe
        typist.reviseSwipe = { _, _ in "in" }
        for letter in "town" { typist.type(String(letter), at: nil, in: field) }
        typist.type(" ", at: nil, in: field)
        precondition(field.text == "in town ")
        typist.delete(in: field)
        precondition(field.text == "on town")
        typist.reviseSwipe = saved
        newField()
        typist.insertSwipe(["word"], uppercase: false, locked: false, in: field)
        typist.type(".", at: nil, in: field)
        precondition(field.text == "word.")
        newField()
    }

    func newField() {
        field = SimField()
        typist.forget()
        keys.reset()
        keys.updateCase(for: field)
        keys.layoutIfNeeded()
    }

    /// A touch, then the layout pass the run loop makes before the next one.
    func tap(at point: CGPoint) {
        keys.tap(at: point)
        keys.layoutIfNeeded()
    }

    /// Rank the committed swipe or the literal text from a tap fallback.
    func gestureChoices(before: String) -> [String] {
        if let choices = typist.swipeChoices(in: field) { return Array(choices.prefix(3)) }
        guard field.text.hasPrefix(before) else { return [] }
        return [String(field.text.dropFirst(before.count)).trimmingCharacters(in: .whitespaces)]
    }

    var traceOutput: FileHandle?

    /// What the suggestion row offers after the latest keystroke.
    /// Types each sentence without mistakes and taps the intended word as soon as the strip offers it.
    /// The spelling typed so far does not count as an offer.
    func scoreSuggestions(_ sentences: [String]) -> String {
        var keys = 0, typedOut = 0, words = 0, nextThree = 0, nextFirst = 0
        func folded(_ text: String) -> String { text.lowercased().replacingOccurrences(of: "’", with: "'") }
        for sentence in sentences {
            var before = ""
            for token in sentence.split(separator: " ") {
                let lead = token.prefix { !$0.isLetter }
                let word = String(token.dropFirst(lead.count).prefix(while: WordInput.wordCharacter))
                guard !word.isEmpty else { before += token + " "; continue }
                before += lead
                words += 1
                typedOut += word.count + 1
                var cost = word.count + 1
                for count in 0..<word.count {
                    let typed = String(word.prefix(count))
                    field.text = before + typed
                    guard let input = WordInput(field) else { continue }
                    let offered = vocabulary.suggestions(for: input).titles.filter { count == 0 || folded($0) != folded(typed) }
                    guard let slot = offered.firstIndex(where: { folded($0) == folded(word) }) else { continue }
                    if count == 0 {
                        nextThree += 1
                        if slot == 0 { nextFirst += 1 }
                    }
                    cost = count + 1
                    break
                }
                keys += cost
                before += token.dropFirst(lead.count) + " "
            }
        }
        func share(_ part: Int, _ whole: Int) -> Double { Double(part) / Double(max(whole, 1)) * 100 }
        return String(format: "words=%d keystrokes_saved=%.1f%% next_word_top3=%.1f%% next_word_top1=%.1f%%",
                      words, share(typedOut - keys, typedOut), share(nextThree, words), share(nextFirst, words))
    }

    func suggestions() -> [String] {
        guard let input = WordInput(field) else { return [] }
        if let choices = typist.swipeChoices(in: field) { return choices }
        let start = DispatchTime.now().uptimeNanoseconds
        let titles = vocabulary.suggestions(for: input, trail: typist.trail, layout: keys.keyLayout).titles
        suggestionNanos.append(DispatchTime.now().uptimeNanoseconds - start)
        return titles
    }
}

private struct Generator {
    private var state: UInt64

    init(_ seed: String) {
        state = 1469598103934665603
        for byte in seed.utf8 { state = (state ^ UInt64(byte)) &* 1099511628211 }
    }

    mutating func next() -> Double {
        state &+= 0x9E3779B97F4A7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58476D1CE4E5B9
        z = (z ^ (z >> 27)) &* 0x94D049BB133111EB
        return Double((z ^ (z >> 31)) >> 11) / Double(1 << 53)
    }

    mutating func gaussian() -> Double {
        let u = max(next(), 1e-12)
        return sqrt(-2 * log(u)) * cos(2 * .pi * next())
    }
}

private struct Level {
    let name: String
    /// Touch spread as a fraction of a letter key's width, on both axes.
    let spread: Double
    /// Chance per letter of each motor slip: a skipped letter, a doubled tap, swapped neighbors.
    let slip: Double
}

private struct Tally {
    var sentences = 0
    var skipped = 0
    var words = 0
    var wordErrors = 0
    var letterTaps = 0
    var letterMisses = 0
    var barChances = 0
    var barHits = 0
}

private func tokens(_ text: String) -> [String] {
    text.lowercased().replacingOccurrences(of: "’", with: "'")
        .split(whereSeparator: \.isWhitespace)
        .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: ".,!?;:\"")) }
        .filter { !$0.isEmpty }
}

private func wordDistance(_ reference: [String], _ hypothesis: [String]) -> Int {
    var row = Array(0...hypothesis.count)
    for (i, word) in reference.enumerated() {
        var diagonal = row[0]
        row[0] = i + 1
        for j in hypothesis.indices {
            let above = row[j + 1]
            row[j + 1] = min(above + 1, row[j] + 1, diagonal + (word == hypothesis[j] ? 0 : 1))
            diagonal = above
        }
    }
    return row[hypothesis.count]
}

@MainActor
private func type(_ sentence: String, on pipeline: Pipeline, level: Level, seed: String, tally: inout Tally, swipe: Bool, straight: Bool) -> Bool {
    let keys = pipeline.keys
    var random = Generator(seed)
    pipeline.newField()
    guard let pitch = keys.frame(of: "q")?.width else { return false }

    func center(_ key: String) -> CGPoint? {
        if let frame = keys.frame(of: key) { return CGPoint(x: frame.midX, y: frame.midY) }
        for page in ["ABC", "123", "#+="] {
            guard let switcher = keys.frame(of: page) else { continue }
            pipeline.tap(at: CGPoint(x: switcher.midX, y: switcher.midY))
            if let frame = keys.frame(of: key) { return CGPoint(x: frame.midX, y: frame.midY) }
        }
        return nil
    }
    func noisy(_ point: CGPoint) -> CGPoint {
        CGPoint(x: point.x + random.gaussian() * level.spread * pitch, y: point.y + random.gaussian() * level.spread * pitch)
    }
    func tapLetter(_ letter: Character) -> Bool {
        let lower = String(letter).lowercased()
        guard let target = center(lower) else { return false }
        if keys.uppercase != letter.isUppercase, let shift = keys.frame(of: "Shift") {
            pipeline.tap(at: CGPoint(x: shift.midX, y: shift.midY))
        }
        let before = pipeline.field.text
        pipeline.tap(at: noisy(target))
        let after = pipeline.field.text
        tally.letterTaps += 1
        if after.count != before.count + 1 || after.last.map({ String($0).lowercased() }) != lower { tally.letterMisses += 1 }
        _ = pipeline.suggestions()
        return true
    }

    let characters = Array(sentence)
    func swipeEnd(_ start: Int) -> Int {
        var end = start
        while end < characters.count {
            let c = characters[end]
            if !c.isLetter && !(c == "'" && end + 1 < characters.count && characters[end + 1].isLetter) { break }
            end += 1
        }
        return end
    }
    func gesture(_ letters: ArraySlice<Character>) -> Bool {
        var via = [CGPoint]()
        var previous: String?
        for c in letters where c.isLetter {
            let key = String(c).lowercased()
            if key == previous { continue }
            guard let target = center(key) else { return false }
            via.append(straight ? noisy(target) : target)
            previous = key
        }
        guard !via.isEmpty else { return false }
        let centers = via
        if !straight && centers.count > 1 {
            for i in centers.indices {
                let a = centers[max(0, i - 1)], b = centers[max(i, 1)]
                let dx = b.x - a.x, dy = b.y - a.y
                let scale = level.spread * pitch / max(1, hypot(dx, dy))
                let along = 1.2 * random.gaussian() * scale
                let across = 0.75 * random.gaussian() * scale
                via[i].x += along * dx - across * dy
                via[i].y += along * dy + across * dx
            }
        }
        let speed = (10 + 10 * random.next()) * pitch
        let hz = 60 + 60 * random.next()
        if via.count > 1 {
            for (i, j) in [(0, 1), (via.count - 1, via.count - 2)] {
                let a = via[i], b = via[j]
                let scale = random.gaussian() * level.spread * pitch / max(1, hypot(a.x-b.x, a.y-b.y))
                via[i] = CGPoint(x: a.x + scale*(a.x-b.x), y: a.y + scale*(a.y-b.y))
            }
        }
        var points = [via[0]]
        func slows(at i: Int) -> Bool {
            if straight || i == 0 || i == centers.count - 1 { return true }
            let a = centers[i-1], b = centers[i], c = centers[i+1]
            let dx = b.x-a.x, dy = b.y-a.y, ex = c.x-b.x, ey = c.y-b.y
            return dx*ex + dy*ey < 0.8 * hypot(dx,dy) * hypot(ex,ey)
        }
        for i in via.indices.dropLast() {
            let a = via[i], b = via[i+1]
            let bow = straight || level.spread == 0 ? 0 : random.gaussian() * (level.name == "steady" ? 0.03 : 0.06)
            let slowStart = slows(at: i), slowEnd = slows(at: i + 1)
            let count = max(2, Int(ceil(hypot(b.x-a.x,b.y-a.y) / speed * hz)))
            for step in 1...count {
                let u = Double(step) / Double(count)
                var t = u*u*(3-2*u)
                if !slowStart { t += u*(1-u)*(1-u) }
                if !slowEnd { t -= u*u*(1-u) }
                let offset = 4*t*(1-t)*bow
                points.append(CGPoint(x: a.x + t*(b.x-a.x) - offset*(b.y-a.y),
                                      y: a.y + t*(b.y-a.y) + offset*(b.x-a.x)))
            }
        }
        let radius = level.spread == 0 ? 0 : Int(2 + 2 * random.next())
        let samples = points.indices.map { i -> SwipeSample in
            var point = points[i]
            if i > 0 && i + 1 < points.count {
                let window = points[max(0,i-radius)...min(points.count-1,i+radius)]
                point = CGPoint(x: window.reduce(0) { $0 + $1.x } / Double(window.count),
                                y: window.reduce(0) { $0 + $1.y } / Double(window.count))
            }
            return SwipeSample(point: point, time: Double(i)/hz)
        }
        let oldCount = pipeline.swipeNanos.count
        let before = pipeline.field.text
        let exportLayout = pipeline.traceOutput.map { _ in keys.keyLayout }
        keys.swipe(samples)
        if let file = pipeline.traceOutput, let layout = exportLayout {
            // Normalize the three letter rows to the encoder's unit square.
            let present = layout.centers.compactMap { $0 }
            let top = present.map(\.y).min()!, bottom = present.map(\.y).max()!
            let row = (bottom - top) / 2
            func xy(_ p: CGPoint) -> [Double] {
                [p.x / keys.bounds.width, (p.y - top + row / 2) / (3 * row)]
            }
            var centers = [String: [Double]]()
            for (i, point) in layout.centers.enumerated() {
                if let point { centers[String(Lexicon.alphabet[i])] = xy(point) }
            }
            let trace: [String: Any] = ["word": String(letters), "before": before, "level": level.name, "sentenceID": seed,
                "centers": centers, "points": samples.map { xy($0.point) + [$0.time * 1000] },
                "baseline": pipeline.gestureChoices(before: before)]
            file.write(try! JSONSerialization.data(withJSONObject: trace))
            file.write(Data([10]))
        }
        if letters.count == 10 && pipeline.swipeNanos.count > oldCount {
            pipeline.tenLetterNanos.append(pipeline.swipeNanos.last!)
        }
        keys.layoutIfNeeded()
        return true
    }
    var index = 0
    while index < characters.count {
        let character = characters[index]
        if swipe, character == " ", index + 1 < characters.count,
           characters[index + 1].isLetter, swipeEnd(index + 1) > index + 2 {
            index += 1
            continue
        }
        if swipe, character.isLetter {
            let end = swipeEnd(index)
            if end > index + 1 {
                guard gesture(characters[index..<end]) else { return false }
                index = end
                continue
            }
            guard tapLetter(character) else { return false }
        } else if character.isLetter {
            let roll = random.next()
            if roll < level.slip {
                index += 1
                continue
            } else if roll < level.slip * 2 {
                guard tapLetter(character) else { return false }
            } else if roll < level.slip * 3, index + 1 < characters.count, characters[index + 1].isLetter {
                guard tapLetter(characters[index + 1]), tapLetter(character) else { return false }
                index += 2
                continue
            }
            guard tapLetter(character) else { return false }
        } else if character == " " {
            let reference = tokens(String(characters[..<index])).last
            if let reference, characters[index - 1].isLetter {
                tally.barChances += 1
                if pipeline.suggestions().contains(where: { tokens($0).first == reference }) { tally.barHits += 1 }
            }
            guard let space = center("Space") else { return false }
            pipeline.tap(at: noisy(space))
        } else {
            let key = character == "'" ? "’" : String(character)
            guard let target = center(key) else { return false }
            pipeline.tap(at: target)
        }
        index += 1
    }
    return true
}

private struct FutoLayout: Decodable {
    struct Key: Decodable { let letter: String; let cx, cy: Double }
    let keys: [Key]
}

private struct RealSwipe: Decodable {
    struct Point: Decodable { let x, y, t: Double }
    let id, word_idx: Int
    let word, sentence: String
    let data: [Point]
}

@MainActor
private func replay(_ input: String, layout: String, output: String, pipeline: Pipeline, encoderOnly: Bool, geometryOnly: Bool) throws {
    let decoder = JSONDecoder()
    let source = try decoder.decode(FutoLayout.self, from: Data(contentsOf: URL(fileURLWithPath: layout)))
    // Solve the two least-squares systems for target x and y.
    var gram = simd_double3x3()
    var sx = SIMD3<Double>.zero, sy = SIMD3<Double>.zero
    let centers = pipeline.keys.keyLayout.centers
    for key in source.keys {
        guard let c = key.letter.first, let i = Lexicon.codes[c], let point = centers[i] else { continue }
        let p = SIMD3(key.cx, key.cy, 1)
        gram += simd_double3x3(columns: (p * p.x, p * p.y, p))
        sx += p * point.x
        sy += p * point.y
    }
    guard abs(gram.determinant) > 1e-8 else { throw CocoaError(.fileReadCorruptFile) }
    let ax = gram.inverse * sx, ay = gram.inverse * sy
    var rows = ["id\tword\ttop1\ttop2\ttop3"]
    let inputText = try Data(contentsOf: URL(fileURLWithPath: input), options: .mappedIfSafe)
    // Geometry needs the physical row-to-column pitch ratio; neural inputs use a unit square.
    let rowSpan = (centers[Lexicon.codes["z"]!]!.y - centers[Lexicon.codes["q"]!]!.y) * 1.5
    let rowScale = rowSpan / pipeline.keys.bounds.width
    var normalizedCenters = [CGPoint?](repeating: nil, count: Lexicon.alphabet.count)
    for key in source.keys {
        if let letter = key.letter.first, let code = Lexicon.codes[letter] {
            normalizedCenters[code] = CGPoint(x: key.cx, y: key.cy * rowScale)
        }
    }
    var inputLayout = KeyLayout()
    inputLayout.centers = normalizedCenters
    inputLayout.reach = CGRect(x: 0, y: 0, width: 1, height: 1)
    inputLayout.pitch = normalizedCenters[Lexicon.codes["å"]!] == nil ? 0.1 : 1.0 / 11
    let geometryLexicons = geometryOnly ? ["en", "sv"].compactMap { Lexicon(language: $0) } : []
    if encoderOnly {
        pipeline.vocabulary.configure(inputLayout.pitch == 0.1 ? .english : .bilingual, dictionaries: [])
        guard geometryOnly || NeuralSwipe.encoder != nil else { throw CocoaError(.fileReadCorruptFile) }
    }
    var elapsed = [Double]()
    var correct = 0
    for line in inputText.split(separator: UInt8(10)) {
        let record = try decoder.decode(RealSwipe.self, from: line)
        let context = record.sentence.split(whereSeparator: \.isWhitespace).prefix(record.word_idx).joined(separator: " ")
        if encoderOnly {
            let start = CFAbsoluteTimeGetCurrent()
            let samples = record.data.map { SwipeSample(point: CGPoint(x: $0.x, y: $0.y * rowScale), time: $0.t / 1000) }
            if geometryOnly {
                let decoder = SwipeDecoder(samples: samples, layout: inputLayout)
                let tail = context.suffix(128).reversed().prefix { !$0.isNewline && !".!?".contains($0) }.reversed()
                let previous = tail.split { !WordInput.wordCharacter($0) }.last.map {
                    String($0).lowercased().replacingOccurrences(of: "’", with: "'")
                }
                func candidates(relaxed: Bool) -> [[[String: Any]]] {
                    geometryLexicons.map { lex in
                        let prior = previous == nil ? lex.sentenceStart : previous.flatMap(lex.node).flatMap(lex.terminal)
                        return decoder.candidates(in: lex, after: prior, relaxed: relaxed).map { ["word": $0.0, "score": $0.1] }
                    }
                }
                let strict = candidates(relaxed: false)
                let relaxed = strict.allSatisfy(\.isEmpty) ? candidates(relaxed: true) : [[], []]
                let data = try JSONSerialization.data(withJSONObject: ["id": record.id, "strict": strict, "relaxed": relaxed])
                rows.append(String(decoding: data, as: UTF8.self))
                continue
            }
            let ranked = pipeline.vocabulary.swipe(samples, layout: inputLayout, before: context)
                .words.prefix(3).map { $0.word.lowercased() }
            elapsed.append((CFAbsoluteTimeGetCurrent() - start) * 1000)
            let label = record.word.lowercased().replacingOccurrences(of: "’", with: "'")
                .trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
            if ranked.first == label { correct += 1 }
            rows.append(([String(record.id), label] + ranked).joined(separator: "\t"))
            continue
        }
        pipeline.newField()
        let before = context.isEmpty ? "" : context + " "
        pipeline.field.text = before
        pipeline.keys.updateCase(for: pipeline.field)
        pipeline.keys.layoutIfNeeded()
        let samples = record.data.map {
            let p = SIMD3($0.x, $0.y, 1)
            return SwipeSample(point: CGPoint(x: simd_dot(ax, p), y: simd_dot(ay, p)), time: ($0.t - record.data[0].t)/1000)
        }
        if rows.count <= 8 {
            let nearest = [samples.first, samples.last].compactMap { sample -> Character? in
                guard let point = sample?.point else { return nil }
                let i = centers.indices.min {
                    func distance(_ i: Int) -> Double {
                        centers[i].map { hypot($0.x-point.x, $0.y-point.y) } ?? .infinity
                    }
                    return distance($0) < distance($1)
                }!
                return Lexicon.alphabet[i]
            }
            print("trace", record.word, String(nearest))
        }
        pipeline.keys.swipe(samples)
        let choices = pipeline.gestureChoices(before: before)
        rows.append(([String(record.id), record.word] + choices).joined(separator: "\t"))
    }
    if !elapsed.isEmpty {
        elapsed.sort()
        print("neural top1=\(correct)/\(elapsed.count) p50_ms=\(elapsed[elapsed.count/2]) p95_ms=\(elapsed[elapsed.count*95/100]) peak_mib=\(NeuralSwipe.peakMiB)")
    }
    if geometryOnly { rows.removeFirst() }
    try rows.joined(separator: "\n").write(toFile: output, atomically: true, encoding: .utf8)
}

private struct ReplaySentence: Decodable {
    let person, id: String
    let words: [String]
    let taps: [[[Double]]]
    let spaces: [[Double]?]
}

@MainActor
private func replay(_ path: String, out: String, languages: TypingLanguages, size: CGSize, observedSpaces: Bool) async throws {
    let data = try Data(contentsOf: URL(fileURLWithPath: path))
    let sentences = try JSONDecoder().decode([ReplaySentence].self, from: data)
    let lexicons = (languages == .english ? ["en"] : ["en", "sv"]).compactMap { Lexicon(language: $0) }
    var rows = [[String: Any]]()
    var person: String?
    var pipeline: Pipeline!
    for sentence in sentences {
        if person != sentence.person {
            pipeline = await Pipeline(languages: languages, size: size)
            person = sentence.person
        }
        pipeline.newField()
        let bar = pipeline.keys.frame(of: "Space")!
        var choices = [[String]]()
        var beforeSpace = [String]()
        var insertedSpace = [Bool]()
        var commits = [String]()
        precondition(sentence.words.count == sentence.taps.count && sentence.words.count == sentence.spaces.count)
        for (index, taps) in sentence.taps.enumerated() {
            let before = pipeline.field.text
            for point in taps { pipeline.tap(at: CGPoint(x: point[0], y: point[1])) }
            let typed = pipeline.field.text
            beforeSpace.append(typed.hasPrefix(before) ? String(typed.dropFirst(before.count)) : (tokens(typed).last ?? ""))
            choices.append(Array(pipeline.suggestions().prefix(3)))
            if observedSpaces, let space = sentence.spaces[index] {
                pipeline.tap(at: CGPoint(x: space[0], y: space[1]))
            } else {
                pipeline.tap(at: CGPoint(x: bar.midX, y: bar.midY))
            }
            let after = pipeline.field.text
            insertedSpace.append(after.last?.isWhitespace == true && after != typed)
            commits.append(after.hasPrefix(before) ? String(after.dropFirst(before.count)) : (tokens(after).last ?? ""))
        }
        let known = sentence.words.map { word in lexicons.contains { $0.node(word).flatMap($0.terminal) != nil } }
        rows.append(["known": known, "id": sentence.id, "bars": choices, "commits": commits, "text": pipeline.field.text,
                     "beforeSpace": beforeSpace, "insertedSpace": insertedSpace])
    }
    try JSONSerialization.data(withJSONObject: rows).write(to: URL(fileURLWithPath: out))
}

@main
enum TypingSim {
    @MainActor static func main() async throws {
        let arguments = CommandLine.arguments
        func option(_ name: String) -> String? {
            arguments.firstIndex(of: name).flatMap { arguments.indices.contains($0 + 1) ? arguments[$0 + 1] : nil }
        }
        guard let corpus = option("--corpus") else {
            print("usage: TypingSim --corpus <dir> [--set dev|test] [--languages bilingual|english|swedish] [--out <tsv>]")
            exit(2)
        }
        if let path = option("--typing-log") { try await replayPhone(path); return }
        let swipe = option("--mode") == "swipe"
        let gesture = option("--gesture") ?? "curved"
        guard ["straight", "curved"].contains(gesture) else { exit(2) }
        let straight = gesture == "straight"
        let set = option("--set") ?? "dev"
        let mode = option("--languages") ?? "bilingual"
        let languages: TypingLanguages = mode == "english" ? .english : mode == "swedish" ? .swedish : .bilingual
        let files = mode == "english" ? ["en"] : mode == "swedish" ? ["sv"] : ["en", "sv"]
        let size = CGSize(width: 1184.0 / 3, height: 216)
        if let path = option("--replay"), let out = option("--out") {
            try await replay(path, out: out, languages: languages, size: size, observedSpaces: arguments.contains("--observed-spaces"))
            return
        }
        let swipeReplay = ["real-swipe", "neural-swipe", "swipe-geometry"].contains(option("--mode") ?? "")
        if !swipeReplay, let path = option("--layout") {
            let pipeline = await Pipeline(languages: languages, size: size)
            var frames = [String: [Double]]()
            for key in Lexicon.alphabet.map(String.init) + ["Space"] {
                if let rect = pipeline.keys.frame(of: key) {
                    frames[key] = [rect.midX, rect.midY, rect.width, rect.height]
                }
            }
            try JSONEncoder().encode(frames).write(to: URL(fileURLWithPath: path))
            return
        }
        let levels = [
            Level(name: "clean", spread: 0, slip: 0),
            Level(name: "steady", spread: 0.2, slip: 0.005),
            Level(name: "hurried", spread: swipe ? 0.35 : 0.3, slip: 0.005)
        ]
        let chosen = option("--level").map { name in levels.filter { $0.name == name } } ?? levels
        let pipeline = await Pipeline(languages: languages, size: size)
        pipeline.vocabulary.useCanonicalSwipePass = !arguments.contains("--native-only")
        if swipeReplay {
            guard let input = option("--input"), let layout = option("--layout"), let out = option("--out") else { exit(2) }
            pipeline.configureSwipe(endpoints: false)
            do { try replay(input, layout: layout, output: out, pipeline: pipeline, encoderOnly: option("--mode") != "real-swipe", geometryOnly: option("--mode") == "swipe-geometry") }
            catch { print(error); exit(1) }
            return
        }
        if option("--mode") == "suggest" {
            for language in files {
                let path = option("--input") ?? "\(corpus)/\(set)-\(language).txt"
                guard let text = try? String(contentsOfFile: path, encoding: .utf8) else {
                    print("missing \(path)")
                    exit(1)
                }
                print("suggest language=\(language) " + pipeline.scoreSuggestions(text.split(separator: "\n").map(String.init)))
            }
            return
        }
        if swipe {
            pipeline.checkSwipeEdits()
            pipeline.configureSwipe(endpoints: arguments.contains("--endpoints"))
        }
        var rows = ["level\tlanguage\tindex\twords\terrors"]
        for level in chosen {
            pipeline.swipeNanos.removeAll()
            pipeline.tenLetterNanos.removeAll()
            var tally = Tally()
            pipeline.suggestionNanos.removeAll()
            for language in files {
                if let path = option("--export-sv"), language == "sv" {
                    guard FileManager.default.createFile(atPath: path, contents: nil),
                          let output = FileHandle(forWritingAtPath: path) else {
                        print("Could not open trace output at \(path)")
                        exit(1)
                    }
                    pipeline.traceOutput = output
                } else { pipeline.traceOutput = nil }
                let path = option("--input") ?? "\(corpus)/\(set)-\(language).txt"
                guard let text = try? String(contentsOfFile: path, encoding: .utf8) else {
                    print("missing \(path)")
                    exit(1)
                }
                for (index, sentence) in text.split(separator: "\n").map(String.init).enumerated() {
                    guard type(sentence, on: pipeline, level: level, seed: "\(level.name)/\(language)/\(index)", tally: &tally, swipe: swipe, straight: straight) else {
                        tally.skipped += 1
                        continue
                    }
                    let reference = tokens(sentence)
                    let errors = wordDistance(reference, tokens(pipeline.field.text))
                    tally.sentences += 1
                    tally.words += reference.count
                    tally.wordErrors += errors
                    rows.append("\(level.name)\t\(language)\t\(index)\t\(reference.count)\t\(errors)")
                }
            }
            let nanos = (swipe ? pipeline.swipeNanos : pipeline.suggestionNanos).sorted()
            if swipe {
                let ten = pipeline.tenLetterNanos.sorted()
                let p95 = ten.isEmpty ? 0 : Double(ten[Int(Double(ten.count - 1)*0.95)])/1e6
                print("ten_letter level=\(level.name) count=\(ten.count) decode_ms_p95=\(p95)")
            }
            func millis(_ q: Double) -> Double { nanos.isEmpty ? 0 : Double(nanos[Int(Double(nanos.count - 1) * q)]) / 1e6 }
            let wer = Double(tally.wordErrors) / Double(max(tally.words, 1)) * 100
            print(String(
                format: "%@ errors=%d wer=%.2f%% letter_miss=%.2f%% bar_recall=%.1f%% sentences=%d skipped=%d words=%d %@_ms_p50=%.2f p95=%.2f calls=%d",
                level.name, tally.wordErrors, wer, Double(tally.letterMisses) / Double(max(tally.letterTaps, 1)) * 100,
                Double(tally.barHits) / Double(max(tally.barChances, 1)) * 100,
                tally.sentences, tally.skipped, tally.words, swipe ? "decode" : "suggest", millis(0.5), millis(0.95), nanos.count))
        }
        if let out = option("--out") {
            try? (rows.joined(separator: "\n") + "\n").write(toFile: out, atomically: true, encoding: .utf8)
        }
    }
}
