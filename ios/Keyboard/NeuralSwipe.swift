import Foundation
import SwipeRuntime

enum NeuralSwipe {
    static var peakMiB: Double {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout.size(ofValue: info) / MemoryLayout<integer_t>.size)
        let result = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        return result == KERN_SUCCESS ? Double(info.ledger_phys_footprint_peak) / 1048576 : -1
    }

    static let encoder: SwipeEncoder? = {
        guard let path = Bundle.main.path(forResource: "encoder", ofType: "pte") else { return nil }
        do {
            let start = ProcessInfo.processInfo.systemUptime
            let model = try SwipeEncoder(path: path)
            let loadMs = (ProcessInfo.processInfo.systemUptime - start) * 1000
            Diagnostics.log("swipe-model", ["load_ms": loadMs])
            NSLog("FUTO Swipe load %.2f ms", loadMs)
            return model
        }
        catch { NSLog("FUTO Swipe load failed: %@", error.localizedDescription); return nil }
    }()

    private static let rows = ["qwertyuiop", "asdfghjkl", "zxcvbnm"].map { row in
        row.map { Lexicon.codes[$0]! }
    }

    // Centers from the published QWERTY layout. The lower two rows have a fractional-pixel inset.
    private static let qwerty: [CGPoint?] = {
        var centers = [CGPoint?](repeating: nil, count: Lexicon.alphabet.count)
        let left = [0.05, 43.0 / 428, 85.8 / 428]
        for (row, letters) in rows.enumerated() {
            for (column, code) in letters.enumerated() {
                centers[code] = CGPoint(x: left[row] + Double(column) / 10, y: (Double(row) + 0.5) / 3)
            }
        }
        return centers
    }()

    static func emissions(_ samples: [SwipeSample], layout: KeyLayout, canonical: Bool) -> [Float]? {
        let rows = Self.rows.map { $0.compactMap { layout.centers[$0] } }
        guard zip(rows, Self.rows).allSatisfy({ $0.count == $1.count }),
              layout.reach.width > 0 else { return nil }
        let line = rows.map { $0[0].y }
        guard line[0] < line[1], line[1] < line[2] else { return nil }
        func rowNumber(_ y: Double) -> Double {
            let i = y < line[1] ? 0 : 1
            return Double(i) + (y - line[i]) / (line[i+1] - line[i])
        }
        func native(_ p: CGPoint) -> CGPoint {
            CGPoint(x: (p.x - layout.reach.minX) / layout.reach.width,
                    y: ((p.y - line[0]) / ((line[2] - line[0]) / 2) + 0.5) / 3)
        }
        func mappedX(_ x: Double, row: Int) -> Double {
            let points = rows[row]
            let upper = min(points.count - 1, max(1, points.firstIndex { $0.x >= x } ?? points.count - 1))
            let a = points[upper-1].x, b = points[upper].x
            let target = Self.qwerty[Self.rows[row][upper-1]]!.x
            return target + 0.1 * (x - a) / max(0.000001, b - a)
        }
        func mapped(_ p: CGPoint) -> CGPoint {
            let y = rowNumber(p.y)
            let clamped = max(0, min(2, y)), row = min(1, Int(clamped))
            let a = mappedX(p.x, row: row), b = mappedX(p.x, row: row+1)
            return CGPoint(x: a + (b - a) * (clamped - Double(row)), y: (y + 0.5) / 3)
        }
        let points = samples.map { SwipeSample(point: canonical ? mapped($0.point) : native($0.point), time: $0.time) }
        return forward(points, centers: canonical ? qwerty : layout.centers.map { $0.map(native) })
    }

    static func forward(_ samples: [SwipeSample], centers: [CGPoint?]) -> [Float]? {
        guard let first = samples.first, let last = samples.last, let encoder else { return nil }
        let duration = last.time - first.time
        // The reference first interpolates on a 60 Hz clock, then on 64 indices.
        // Evaluate just the two clock samples needed for each output.
        let timed = duration > 0.000001
        let intervals = timed ? max(1, (duration * 60).rounded(.toNearestOrEven)) : Double(samples.count - 1)
        func interpolate(_ a: CGPoint, _ b: CGPoint, _ t: Double) -> CGPoint {
            CGPoint(x: a.x + t * (b.x - a.x), y: a.y + t * (b.y - a.y))
        }
        func at(_ grid: Double) -> CGPoint {
            if !timed { return samples[Int(grid)].point }
            let t = first.time + duration * grid / intervals
            var low = 0, high = samples.count
            while low < high {
                let mid = (low + high) / 2
                if samples[mid].time <= t { low = mid + 1 } else { high = mid }
            }
            guard low > 0, low < samples.count else { return low == 0 ? first.point : last.point }
            let a = samples[low - 1], b = samples[low]
            return interpolate(a.point, b.point, (t - a.time) / (b.time - a.time))
        }
        var features = [Float](repeating: 0, count: 128), keys = features
        var mask = [UInt8](repeating: 0, count: 64)
        for i in 0..<64 {
            let index = intervals * Double(i) / 63
            let point = interpolate(at(floor(index)), at(ceil(index)), index - floor(index))
            features[i] = Float(point.x)
            features[64 + i] = Float(point.y)
        }
        for (i, center) in centers.enumerated() {
            guard let center else { continue }
            keys[2*i] = Float(center.x)
            keys[2*i+1] = Float(center.y)
            mask[i] = 1
        }
        return encoder.emissions(features, keys: keys, mask: mask)?.withUnsafeBytes {
            Array($0.bindMemory(to: Float.self))
        }
    }

    struct Candidate {
        let word: String
        let id: Int
        let mass: Double

        func score(prior: Double, language: Double, frequency: Double, exponent: Double = 0.45,
                   lengthBonus: Double = 1.25, bigramWeight: Double = 0.40,
                   languageWeight: Double = 0.50, frequencyWeight: Double = 0.005) -> Double {
            let length = Double(max(1, word.filter { $0 != "'" }.count))
            return mass / pow(length, exponent) + lengthBonus * length + bigramWeight * prior
                + languageWeight * language + frequencyWeight * frequency
        }
    }

    static func average(_ native: [Candidate], _ canonical: [Candidate]) -> [Candidate] {
        var candidates = Dictionary(uniqueKeysWithValues: native.map { ($0.id, $0) })
        for other in canonical {
            if let first = candidates[other.id] {
                let mass = max(first.mass, other.mass) + log1p(exp(-abs(first.mass - other.mass)))
                candidates[other.id] = Candidate(word: first.word, id: first.id, mass: mass)
            } else {
                candidates[other.id] = other
            }
        }
        return candidates.values.map { Candidate(word: $0.word, id: $0.id, mass: $0.mass - log(2)) }
    }

    private struct Prefix {
        let node: Int
        var blank: Double
        var letter: Double
        var total: Double
    }

    static func beam(_ emissions: [Float], lex: Lexicon) -> [Candidate] {
        func sum(_ a: Double, _ b: Double) -> Double {
            if min(a, b) == -.infinity { return max(a, b) }
            return max(a, b) + log1p(exp(-abs(a-b)))
        }
        var beam = [Prefix(node: 0, blank: 0, letter: -.infinity, total: 0)]
        var next = [Prefix]()
        // Each child has one parent. Only the retained prefixes can collide with an extension.
        for frame in stride(from: 0, to: emissions.count, by: 65) {
            next.removeAll(keepingCapacity: true)
            var retained = [Int: Int]()
            for state in beam {
                retained[state.node] = next.count
                let blank = state.total + Double(emissions[frame + 64])
                let letter = state.letter + Double(emissions[frame + lex.letter(state.node)])
                next.append(Prefix(node: state.node, blank: blank, letter: letter, total: sum(blank, letter)))
            }
            for state in beam {
                let previous = state.node == 0 ? -1 : lex.letter(state.node)
                for child in lex.children(state.node) {
                    let code = lex.letter(child)
                    let mass = (previous == code ? state.blank : state.total) + Double(emissions[frame + code])
                    guard mass.isFinite else { continue }
                    if let i = retained[child] {
                        next[i].letter = sum(next[i].letter, mass)
                        next[i].total = sum(next[i].blank, next[i].letter)
                    } else {
                        next.append(Prefix(node: child, blank: -.infinity, letter: mass, total: mass))
                    }
                }
            }
            next.sort { $0.total == $1.total ? $0.node < $1.node : $0.total > $1.total }
            beam = Array(next.prefix(100))
        }
        return beam.compactMap { state in
            guard state.total.isFinite, let id = lex.terminal(state.node) else { return nil }
            return Candidate(word: lex.text(id), id: id, mass: state.total)
        }
    }
}
