import Foundation

struct KeyLayout: Equatable {
    var centers = [CGPoint?](repeating: nil, count: 30)
    var pitch: CGFloat = 1
    var reach: CGRect = .zero

    static func standard(swedish: Bool) -> Self {
        let rows = [
            swedish ? "qwertyuiopå" : "qwertyuiop", swedish ? "asdfghjklöä" : "asdfghjkl", "zxcvbnm",
        ]
        var layout = Self()
        layout.reach = CGRect(x: 0, y: 0, width: 394, height: 210)
        layout.pitch = layout.reach.width / CGFloat(rows[0].count)
        let height = layout.reach.height / 4
        for (row, letters) in rows.enumerated() {
            let left = (layout.reach.width - CGFloat(letters.count) * layout.pitch) / 2
            for (column, letter) in letters.enumerated() {
                if let code = Lexicon.codes[letter] {
                    layout.centers[code] = CGPoint(
                        x: left + (CGFloat(column) + 0.5) * layout.pitch,
                        y: (CGFloat(row) + 0.5) * height)
                }
            }
        }
        return layout
    }

    func costs(letter: Int, point: CGPoint?, spread: Double) -> [Double] {
        guard let center = centers[letter] else {
            var costs = Array(repeating: Double.infinity, count: centers.count)
            costs[letter] = 0
            return costs
        }
        let p = point ?? center
        let sigma = pitch * (point == nil ? 0.25 : spread)
        let scale = 0.5 / (sigma * sigma)
        return centers.map {
            guard let center = $0 else { return .infinity }
            let x = center.x - p.x
            let y = center.y - p.y
            return (x * x + y * y) * scale
        }
    }

    func omissionCosts(spread: Double) -> [Double] {
        let sigma = pitch * spread
        func inside(_ center: CGFloat, _ low: CGFloat, _ high: CGFloat) -> Double {
            let scale = sigma * sqrt(2.0)
            return 0.5 * (erf((high - center) / scale) - erf((low - center) / scale))
        }
        return centers.map { center in
            guard let center else { return -log(0.005) }
            let p = inside(center.x, reach.minX, reach.maxX) * inside(center.y, reach.minY, reach.maxY)
            return -log(0.005 + 1 - p)
        }
    }
}

final class TapDecoder {
    private struct State {
        let node: Int
        let cost: Double
        let slips: Int
    }

    private let lexicon: Lexicon
    private let slipCost = -log(0.005)
    private var costs: [[Double]] = []
    private var omissions: [Double] = []
    private var doubleCost = -log(0.005)
    private var layers = [[State(node: 0, cost: 0, slips: 0)]]

    init(lexicon: Lexicon) { self.lexicon = lexicon }

    private func advance(_ index: Int) -> [State] {
        let row = costs[index]
        let layer = layers[index]
        let cutoff = (layer.map(\.cost).min() ?? 0) + 9
        var states: [Int: State] = [:]
        func add(_ node: Int, _ cost: Double, _ slips: Int) {
            guard cost.isFinite else { return }
            let key = node * 3 + slips
            if cost < states[key]?.cost ?? .infinity {
                states[key] = State(node: node, cost: cost, slips: slips)
            }
        }
        for state in layer {
            let canSlip = state.slips < 2 && state.cost <= cutoff
            for child in lexicon.children(state.node) {
                let letter = lexicon.letter(child)
                if row[letter] <= 14 { add(child, state.cost + row[letter], state.slips) }
                if canSlip {
                    for grand in lexicon.children(child) {
                        let distance = row[lexicon.letter(grand)]
                        if distance <= 14 {
                            add(grand, state.cost + omissions[letter] + distance, state.slips + 1)
                        }
                    }
                }
            }
            if canSlip && state.node != 0 {
                let distance = row[lexicon.letter(state.node)]
                if distance <= 14 { add(state.node, state.cost + distance + doubleCost, state.slips + 1) }
            }
        }
        if index > 0 {
            let earlier = layers[index - 1]
            let cutoff = (earlier.map(\.cost).min() ?? 0) + 9
            for state in earlier where state.slips < 2 && state.cost <= cutoff {
                for child in lexicon.children(state.node) {
                    let a = lexicon.letter(child)
                    guard row[a] <= 14 else { continue }
                    for grand in lexicon.children(child) {
                        let b = lexicon.letter(grand)
                        let prior = costs[index - 1][b]
                        if b != a && prior <= 14 {
                            add(grand, state.cost + slipCost + row[a] + prior, state.slips + 1)
                        }
                    }
                }
            }
        }
        let ranked = states.values.map { ($0, $0.cost - lexicon.best($0.node)) }
        let bound = (ranked.map { $0.1 }.min() ?? 0) + 14
        let kept = ranked.filter { $0.1 <= bound }
        if kept.count <= 160 { return kept.map { $0.0 } }
        return kept.sorted {
            if $0.1 != $1.1 { return $0.1 < $1.1 }
            if $0.0.node != $1.0.node { return $0.0.node < $1.0.node }
            return $0.0.slips < $1.0.slips
        }.prefix(160).map { $0.0 }
    }

    func candidates(costs next: [[Double]], omissions: [Double], textOnly: Bool) -> [Int: Double] {
        // Text without touch geometry keeps the existing preference for collapsing repeats.
        let repeatPenalty = textOnly ? 3.8 : slipCost
        let shared =
            self.omissions == omissions && doubleCost == repeatPenalty
            ? zip(costs, next).prefix { $0 == $1 }.count : 0
        layers.removeLast(layers.count - shared - 1)
        doubleCost = repeatPenalty
        costs = next
        self.omissions = omissions
        for i in shared..<next.count { layers.append(advance(i)) }
        var result: [Int: Double] = [:]
        for state in layers.last! {
            if let id = lexicon.terminal(state.node) {
                result[id] = min(result[id] ?? .infinity, state.cost)
            }
            if state.slips >= 2 { continue }
            for child in lexicon.children(state.node) {
                if let id = lexicon.terminal(child) {
                    result[id] = min(result[id] ?? .infinity, state.cost + omissions[lexicon.letter(child)])
                }
            }
        }
        return result
    }
}
