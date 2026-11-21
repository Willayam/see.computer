import UIKit

/// Types held-out sentences on the real keyboard views with simulated finger noise
/// and scores the words that end up in the field.
@MainActor
private final class Field: NSObject, UITextDocumentProxy {
    var text = ""
    var documentContextBeforeInput: String? { text }
    var documentContextAfterInput: String? { nil }
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
    private(set) var field = Field()
    private var typist: Typist!
    var suggestionNanos: [UInt64] = []

    init(languages: TypingLanguages, size: CGSize) async {
        keys.frame = CGRect(origin: .zero, size: size)
        keys.configure(swedish: languages.usesSwedishKeys, needsGlobe: true)
        keys.layoutIfNeeded()
        vocabulary.configure(languages, dictionaries: UITextChecker.availableLanguages)
        await withCheckedContinuation { continuation in vocabulary.load { continuation.resume() } }
        typist = Typist { [vocabulary] in vocabulary.suggestions(for: $0) }
        keys.insert = { [unowned self] output in
            self.typist.type(output, in: self.field)
            self.keys.updateCase(for: self.field)
        }
    }

    func newField() {
        field = Field()
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

    /// What the suggestion row offers after the latest keystroke.
    func suggestions() -> [String] {
        guard let input = WordInput(field) else { return [] }
        let start = DispatchTime.now().uptimeNanoseconds
        let titles = vocabulary.suggestions(for: input).titles
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
private func type(_ sentence: String, on pipeline: Pipeline, level: Level, seed: String, tally: inout Tally) -> Bool {
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
    var index = 0
    while index < characters.count {
        let character = characters[index]
        if character.isLetter {
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

@main
enum TypingSim {
    @MainActor static func main() async {
        let arguments = CommandLine.arguments
        func option(_ name: String) -> String? {
            arguments.firstIndex(of: name).flatMap { arguments.indices.contains($0 + 1) ? arguments[$0 + 1] : nil }
        }
        guard let corpus = option("--corpus") else {
            print("usage: TypingSim --corpus <dir> [--set dev|test] [--languages bilingual|english] [--out <tsv>]")
            exit(2)
        }
        let set = option("--set") ?? "dev"
        let mode = option("--languages") ?? "bilingual"
        let languages: TypingLanguages = mode == "english" ? .english : .bilingual
        let files = mode == "english" ? ["en"] : ["en", "sv"]
        let size = CGSize(width: 394, height: 210)
        let levels = [
            Level(name: "clean", spread: 0, slip: 0),
            Level(name: "steady", spread: 0.2, slip: 0.005),
            Level(name: "hurried", spread: 0.3, slip: 0.005)
        ]
        let chosen = option("--level").map { name in levels.filter { $0.name == name } } ?? levels
        let pipeline = await Pipeline(languages: languages, size: size)
        var rows = ["level\tlanguage\tindex\twords\terrors"]
        for level in chosen {
            var tally = Tally()
            pipeline.suggestionNanos.removeAll()
            for language in files {
                let path = "\(corpus)/\(set)-\(language).txt"
                guard let text = try? String(contentsOfFile: path, encoding: .utf8) else {
                    print("missing \(path)")
                    exit(1)
                }
                for (index, sentence) in text.split(separator: "\n").map(String.init).enumerated() {
                    guard type(sentence, on: pipeline, level: level, seed: "\(level.name)/\(language)/\(index)", tally: &tally) else {
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
            let nanos = pipeline.suggestionNanos.sorted()
            func millis(_ q: Double) -> Double { nanos.isEmpty ? 0 : Double(nanos[Int(Double(nanos.count - 1) * q)]) / 1e6 }
            let wer = Double(tally.wordErrors) / Double(max(tally.words, 1)) * 100
            print(String(
                format: "%@ errors=%d wer=%.2f%% letter_miss=%.2f%% bar_recall=%.1f%% sentences=%d skipped=%d words=%d suggest_ms_p50=%.2f p95=%.2f calls=%d",
                level.name, tally.wordErrors, wer, Double(tally.letterMisses) / Double(max(tally.letterTaps, 1)) * 100,
                Double(tally.barHits) / Double(max(tally.barChances, 1)) * 100,
                tally.sentences, tally.skipped, tally.words, millis(0.5), millis(0.95), nanos.count))
        }
        if let out = option("--out") {
            try? (rows.joined(separator: "\n") + "\n").write(toFile: out, atomically: true, encoding: .utf8)
        }
    }
}
