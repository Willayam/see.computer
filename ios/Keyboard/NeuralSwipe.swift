import Foundation
import SwipeRuntime

enum NeuralSwipe {
    static let warpToQwerty = false
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

    // Map the three letter rows to the coordinate space used by the encoder.
    static func emissions(_ samples: [SwipeSample], layout: KeyLayout, warp: Bool = warpToQwerty) -> [Float]? {
        func center(_ letter: Character) -> CGPoint? { layout.centers[Lexicon.codes[letter]!] }
        guard let q = center("q"), let p = center("p"), let a = center("a"), let z = center("z"),
              layout.pitch > 0, z.y > q.y, p.x > q.x else { return nil }
        let width = layout.pitch * (center("å") == nil ? 10 : 11)
        func normalized(_ pt: CGPoint) -> CGPoint {
            let row = 2 * (pt.y - q.y) / (z.y - q.y)
            var x = (pt.x - q.x + layout.pitch / 2) / width
            if warp {
                let r = max(0, min(2, row))
                let left = r < 1 ? q.x + r * (a.x - q.x) : a.x + (r - 1) * (z.x - a.x)
                let offset = r < 1 ? 0.05 + 0.05 * r : 0.1 + 0.1 * (r - 1)
                x = offset + (pt.x - left) * 0.9 / (p.x - q.x)
            }
            return CGPoint(x: x, y: (row + 0.5) / 3)
        }
        return forward(samples.map { SwipeSample(point: normalized($0.point), time: $0.time) },
                       centers: layout.centers.map { $0.map(normalized) })
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

        func score(prior: Double, lengthExponent: Double = 0.30, priorWeight: Double = 0.35) -> Double {
            mass / pow(Double(word.count), lengthExponent) + priorWeight * prior
        }
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
