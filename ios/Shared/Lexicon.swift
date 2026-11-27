import Foundation

struct Lexicon {
    static let alphabet = Array("abcdefghijklmnopqrstuvwxyzåäö'")
    static let codes = Dictionary(uniqueKeysWithValues: alphabet.enumerated().map { ($1, $0) })
    private let frequency: Data?
    private let data: Data
    private let nodes, wordNodes, unigrams, rows, backoff, pairs, chars: Int
    private let tripleRows, contexts, contextStarts, triples: Int
    let sentenceStart: Int

    init?(language: String, bundle: Bundle = .main) {
        guard let url = bundle.url(forResource: "words-\(language)", withExtension: "bin"),
            let mapped = try? Data(contentsOf: url, options: .alwaysMapped), mapped.count >= 64,
            mapped.starts(with: "SCLX".utf8)
        else { return nil }
        func uint(at offset: Int) -> Int {
            mapped.withUnsafeBytes { Int($0.loadUnaligned(fromByteOffset: offset, as: UInt32.self)) }
        }
        func field(_ i: Int) -> Int { uint(at: i * 4) }
        let sections = (5...15).map(field)
        // The last context offset counts the triples, which run to the end of the file.
        guard field(1) == 2, sections[0] >= 64, zip(sections, sections.dropFirst()).allSatisfy({ $0 <= $1 }),
              field(11) + 31 * 31 * 31 == field(12), field(14) + 4 <= field(15), field(15) <= mapped.count,
              field(15) + 4 * uint(at: field(15) - 4) == mapped.count else { return nil }
        let frequencies = bundle.url(forResource: "frequency-\(language)", withExtension: "bin")
            .flatMap { try? Data(contentsOf: $0, options: .alwaysMapped) }
        frequency = frequencies?.count == field(3) ? frequencies : nil
        data = mapped
        sentenceStart = field(3)
        nodes = field(5)
        wordNodes = field(6)
        unigrams = field(7)
        rows = field(8)
        backoff = field(9)
        pairs = field(10)
        chars = field(11)
        tripleRows = field(12)
        contexts = field(13)
        contextStarts = field(14)
        triples = field(15)
    }

    private func uint(_ offset: Int) -> Int {
        data.withUnsafeBytes { Int($0.loadUnaligned(fromByteOffset: offset, as: UInt32.self)) }
    }

    private func firstChild(_ n: Int) -> Int { uint(nodes + n * 8) & 0xffffff }
    func children(_ n: Int) -> Range<Int> { firstChild(n)..<firstChild(n + 1) }
    func letter(_ n: Int) -> Int { uint(nodes + n * 8) >> 24 & 31 }
    func terminal(_ n: Int) -> Int? {
        uint(nodes + n * 8) & (1 << 29) == 0 ? nil : uint(nodes + n * 8 + 4) & 0xfffff
    }
    func mass(_ n: Int) -> Double { exp(-Double(uint(nodes + n * 8 + 4) >> 26) / 2) }
    func best(_ n: Int) -> Double { -Double(uint(nodes + n * 8 + 4) >> 20 & 63) / 2 }

    func node(_ word: String) -> Int? {
        var n = 0
        for ch in word {
            guard let code = Self.codes[ch], let child = children(n).first(where: { letter($0) == code })
            else { return nil }
            n = child
        }
        return n
    }

    /// The nodes that spell a word, from its first letter to its last.
    func path(_ id: Int) -> [Int] {
        var n = uint(wordNodes + 4 * id)
        var path: [Int] = []
        while n > 0 {
            path.append(n)
            var low = 0
            var high = n
            while low + 1 < high {
                let mid = (low + high) / 2
                if firstChild(mid) <= n { low = mid } else { high = mid }
            }
            n = low
        }
        return path.reversed()
    }

    func text(_ id: Int) -> String { String(path(id).map { Self.alphabet[letter($0)] }) }

    func displayForm(_ word: String) -> String {
        guard let n = node(word), uint(nodes + n * 8) & (1 << 30) != 0 else { return word }
        return word.prefix(1).uppercased() + word.dropFirst()
    }

    func sourceFrequency(_ id: Int) -> Double { Double(frequency?[id] ?? 0) }
    func unigram(_ id: Int) -> Double { -Double(data[unigrams + id]) / 10 }
    private func pairRange(_ prev: Int) -> Range<Int> {
        uint(rows + prev * 4)..<uint(rows + (prev + 1) * 4)
    }
    /// The first entry of a sorted run whose low 20 bits are not below `id`.
    private func lowerBound(_ id: Int, in range: Range<Int>, of section: Int) -> Int {
        var low = range.lowerBound
        var high = range.upperBound
        while low < high {
            let mid = (low + high) / 2
            if uint(section + mid * 4) & 0xfffff < id { low = mid + 1 } else { high = mid }
        }
        return low
    }

    private func seen(_ id: Int, in range: Range<Int>, of section: Int, over backed: Double) -> Double {
        let found = lowerBound(id, in: range, of: section)
        guard found < range.upperBound else { return backed }
        let entry = uint(section + found * 4)
        guard entry & 0xfffff == id else { return backed }
        return log(exp(backed) + exp(-Double(entry >> 20) / 128))
    }

    /// Where the table keeps the words seen after `before` and then `prev`.
    private func context(_ before: Int?, _ prev: Int) -> Int? {
        guard let before, prev < sentenceStart else { return nil }
        let range = uint(tripleRows + prev * 4)..<uint(tripleRows + (prev + 1) * 4)
        let found = lowerBound(before, in: range, of: contexts)
        return found < range.upperBound && uint(contexts + found * 4) & 0xfffff == before ? found : nil
    }

    private func tripleRange(_ context: Int) -> Range<Int> {
        uint(contextStarts + context * 4)..<uint(contextStarts + (context + 1) * 4)
    }

    /// `before` is the word ahead of `prev`. It sharpens the estimate when the three were seen together.
    func logProbability(_ id: Int, after prev: Int?, _ before: Int? = nil) -> Double {
        let base = unigram(id)
        guard let prev else { return base }
        let pair = seen(id, in: pairRange(prev), of: pairs, over: base - Double(data[backoff + prev]) / 10)
        guard let context = context(before, prev) else { return pair }
        let backed = pair - Double(uint(contexts + context * 4) >> 20) / 10
        return seen(id, in: tripleRange(context), of: triples, over: backed)
    }

    /// Every word seen after `prev`, or after `before` and `prev`, with a probability of at least `exp(-floor)`.
    func followers(_ prev: Int, _ before: Int?, within floor: Double) -> Set<Int> {
        let limit = Int(floor * 128)
        func likely(_ range: Range<Int>, of section: Int) -> [Int] {
            range.compactMap { offset in
                let entry = uint(section + offset * 4)
                return entry >> 20 <= limit ? entry & 0xfffff : nil
            }
        }
        var words = Set(likely(pairRange(prev), of: pairs))
        if let context = context(before, prev) { words.formUnion(likely(tripleRange(context), of: triples)) }
        return words
    }

    func completions(_ prefix: String, limit: Int) -> [String] {
        guard let n = node(prefix) else { return [] }
        var pending = Array(children(n))
        var top: [Int] = []
        while let child = pending.popLast() {
            if top.count == limit, best(child) + 0.3 < unigram(top.last!) { continue }
            if let id = terminal(child) {
                top.insert(id, at: top.firstIndex { $0 > id } ?? top.count)
                if top.count > limit { top.removeLast() }
            }
            pending.append(contentsOf: children(child))
        }
        return top.map(text)
    }

    func unknown(_ word: String) -> Double {
        var a = 30
        var b = 30
        var sum = 0
        for ch in word {
            guard let c = Self.codes[ch] else { return -.infinity }
            sum += Int(data[chars + (a * 31 + b) * 31 + c])
            a = b
            b = c
        }
        return -Double(sum + Int(data[chars + (a * 31 + b) * 31 + 30])) / 10
    }
}
