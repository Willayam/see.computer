import Foundation

struct Lexicon {
    static let alphabet = Array("abcdefghijklmnopqrstuvwxyzåäö'")
    static let codes = Dictionary(uniqueKeysWithValues: alphabet.enumerated().map { ($1, $0) })
    private let data: Data
    private let nodes, wordNodes, unigrams, rows, backoff, pairs, chars: Int
    let sentenceStart: Int

    init?(language: String) {
        guard let url = Bundle.main.url(forResource: "words-\(language)", withExtension: "bin"),
            let mapped = try? Data(contentsOf: url, options: .alwaysMapped), mapped.count >= 64,
            mapped.starts(with: "SCLX".utf8)
        else { return nil }
        func field(_ i: Int) -> Int {
            mapped.withUnsafeBytes { Int($0.loadUnaligned(fromByteOffset: i * 4, as: UInt32.self)) }
        }
        guard field(1) == 1, field(11) + 31 * 31 * 31 == mapped.count else { return nil }
        data = mapped
        sentenceStart = field(3)
        nodes = field(5)
        wordNodes = field(6)
        unigrams = field(7)
        rows = field(8)
        backoff = field(9)
        pairs = field(10)
        chars = field(11)
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

    func text(_ id: Int) -> String {
        var n = uint(wordNodes + 4 * id)
        var letters: [Character] = []
        while n > 0 {
            letters.append(Self.alphabet[letter(n)])
            var low = 0
            var high = n
            while low + 1 < high {
                let mid = (low + high) / 2
                if firstChild(mid) <= n { low = mid } else { high = mid }
            }
            n = low
        }
        return String(letters.reversed())
    }

    func displayForm(_ word: String) -> String {
        guard let n = node(word), uint(nodes + n * 8) & (1 << 30) != 0 else { return word }
        return word.prefix(1).uppercased() + word.dropFirst()
    }

    func unigram(_ id: Int) -> Double { -Double(data[unigrams + id]) / 10 }
    private func pairRange(_ prev: Int) -> Range<Int> {
        uint(rows + prev * 4)..<uint(rows + (prev + 1) * 4)
    }
    func logProbability(_ id: Int, after prev: Int?) -> Double {
        let base = unigram(id)
        guard let prev else { return base }
        let range = pairRange(prev)
        var low = range.lowerBound
        var high = range.upperBound
        while low < high {
            let mid = (low + high) / 2
            if uint(pairs + mid * 4) & 0xfffff < id { low = mid + 1 } else { high = mid }
        }
        let backed = base - Double(data[backoff + prev]) / 10
        guard low < range.upperBound else { return backed }
        let pair = uint(pairs + low * 4)
        guard pair & 0xfffff == id else { return backed }
        return log(exp(backed) + exp(-Double(pair >> 20) / 128))
    }

    func followers(_ prev: Int, limit: Int) -> [String] {
        var top: [(id: Int, cost: Int)] = []
        for offset in pairRange(prev) {
            let pair = uint(pairs + offset * 4)
            let cost = pair >> 20
            if top.count == limit && cost >= top.last!.cost { continue }
            top.insert((pair & 0xfffff, cost), at: top.firstIndex { $0.cost > cost } ?? top.count)
            if top.count > limit { top.removeLast() }
        }
        return top.map { text($0.id) }
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
