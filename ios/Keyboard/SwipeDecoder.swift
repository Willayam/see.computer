import Foundation

/// Reduce a stroke to endpoints and turns, in units of one key pitch.
struct SwipeDecoder {
    private let path: [CGPoint]
    private let timedPath: [CGPoint]
    private let centers: [CGPoint?]

    init(samples: [SwipeSample], layout: KeyLayout) {
        func scaled(_ p: CGPoint) -> CGPoint { CGPoint(x: p.x / layout.pitch, y: p.y / layout.pitch) }
        centers = layout.centers.map { $0.map(scaled) }
        let points = samples.map { scaled($0.point) }
        guard points.count >= 2 else { path = points; timedPath = []; return }
        var turns = [points[0]]
        func simplify(_ a: Int, _ b: Int, depth: Int) {
            let start = points[a], end = points[b]
            let dx = end.x - start.x, dy = end.y - start.y
            let length = max(0.001, dx * dx + dy * dy)
            var split = a, largest = 0.26 * 0.26
            if depth < 8 {
                for i in (a + 1)..<b {
                    let x = points[i].x - start.x, y = points[i].y - start.y
                    let t = min(1, max(0, (x * dx + y * dy) / length))
                    let error = pow(x - t * dx, 2) + pow(y - t * dy, 2)
                    if error > largest { split = i; largest = error }
                }
            }
            if split != a {
                simplify(a, split, depth: depth + 1)
                simplify(split, b, depth: depth + 1)
            } else { turns.append(end) }
        }
        simplify(0, points.count - 1, depth: 0)
        path = turns
        // A speed valley can identify a letter even when the stroke continues in a straight line.
        let speeds = zip(samples, samples.dropFirst()).map { a, b in
            hypot(b.point.x - a.point.x, b.point.y - a.point.y) / max(0.001, b.time - a.time)
        }
        var anchors = [0]
        if speeds.count >= 5 {
            for i in 2..<(speeds.count - 2) {
                guard speeds[i] < speeds[i - 1], speeds[i] <= speeds[i + 1] else { continue }
                let left = speeds[max(0, i - 8)..<i].max() ?? 0
                let right = speeds[(i + 1)...min(speeds.count - 1, i + 8)].max() ?? 0
                if speeds[i] < 0.65 * min(left, right) { anchors.append(i) }
            }
        }
        anchors.append(points.count - 1)
        turns = [points[0]]
        for (a, b) in zip(anchors, anchors.dropFirst()) {
            simplify(a, b, depth: 0)
        }
        timedPath = turns
    }

    private struct State {
        let node: Int
        let segment: Int
        let fraction: Double
        let cost: Double
        let priority: Double
    }

    func candidates(in lex: Lexicon, after previous: Int?) -> [(String, Double)] {
        let geometric = candidates(along: path, passingLetters: true, in: lex, after: previous)
        guard timedPath.count >= 2 else { return geometric }
        let timed = candidates(along: timedPath, passingLetters: false, in: lex, after: previous)
        var scores = Dictionary(geometric, uniquingKeysWith: max)
        for (word, timedScore) in timed {
            let score = timedScore + (timedPath.count > path.count ? 1.5 : 0)
            if let prior = scores[word] {
                let best = max(prior, score)
                scores[word] = best + log1p(exp(min(prior, score) - best))
            } else {
                scores[word] = score
            }
        }
        return scores.map { ($0.key, $0.value) }
    }

    private func candidates(along path: [CGPoint], passingLetters: Bool, in lex: Lexicon,
                            after previous: Int?) -> [(String, Double)] {
        guard path.count >= 2 else { return [] }
        var beam = [State(node: 0, segment: 0, fraction: 0, cost: 0, priority: 0)]
        var top = [(id: Int, score: Double)]()
        let end = path.count - 1
        for depth in 0..<36 {
            var next = [State]()
            for state in beam {
                for child in lex.children(state.node) {
                    let letter = lex.letter(child)
                    func advance(_ segment: Int, _ fraction: Double, _ penalty: Double) {
                        let cost = state.cost + penalty
                        if segment == end, depth > 0, let id = lex.terminal(child) {
                            let score = lex.logProbability(id, after: previous) - cost
                            if top.count < 3 || score > top.last!.score {
                                if let index = top.firstIndex(where: { $0.id == id }) {
                                    if top[index].score >= score { return }
                                    top.remove(at: index)
                                }
                                top.append((id, score))
                                top.sort { $0.score > $1.score }
                                if top.count > 3 { top.removeLast() }
                            }
                        }
                        let rank = cost - 3 * Double(segment) - lex.best(child) * 0.45
                        next.append(State(node: child, segment: segment, fraction: fraction, cost: cost, priority: rank))
                    }
                    if state.node != 0 && (letter == lex.letter(state.node) || letter == 29) {
                        advance(state.segment, state.fraction, 0.45)
                        continue
                    }
                    guard let center = centers[letter] else { continue }
                    func distance(_ p: CGPoint) -> Double {
                        let x = p.x - center.x, y = p.y - center.y
                        return x * x + y * y
                    }
                    if depth == 0 {
                        let d = distance(path[0])
                        if d < 3 { advance(0, 0, 5 * d) }
                        continue
                    }
                    let i = state.segment
                    guard i < end else { continue }
                    let a = path[i], b = path[i + 1]
                    let d = distance(b)
                    if d < 3.2 { advance(i + 1, 0, 4.8 * d) }
                    guard passingLetters else { continue }
                    // Letters passed along a straight leg need no turn, but must occur in order.
                    let dx = b.x - a.x, dy = b.y - a.y
                    let t = ((center.x - a.x) * dx + (center.y - a.y) * dy) / max(0.001, dx * dx + dy * dy)
                    if t > state.fraction + 0.02 && t < 0.95 {
                        let lateral = distance(CGPoint(x: a.x + t * dx, y: a.y + t * dy))
                        if lateral < 0.85 { advance(i, t, 4 * lateral + 1.5) }
                    }
                }
            }
            if next.isEmpty { break }
            next.sort {
                if $0.priority != $1.priority { return $0.priority < $1.priority }
                return $0.node < $1.node
            }
            beam = Array(next.prefix(80))
        }
        var candidates = top.map { (lex.displayForm(lex.text($0.id)), $0.score) }
        var spelling = ""
        var cost = 0.0
        for point in path {
            var nearest = 0, distance = Double.infinity
            for (letter, center) in centers.enumerated() {
                guard let center else { continue }
                let dx = point.x - center.x, dy = point.y - center.y
                let d = dx * dx + dy * dy
                if d < distance { nearest = letter; distance = d }
            }
            guard distance < 1 else { return candidates }
            let letter = Lexicon.alphabet[nearest]
            if spelling.last != letter { spelling.append(letter) }
            cost += 5 * distance
        }
        if spelling.count >= 2, lex.node(spelling).flatMap(lex.terminal) == nil {
            candidates.append((spelling, lex.unknown(spelling) - 6 - cost))
        }
        return candidates
    }
}
