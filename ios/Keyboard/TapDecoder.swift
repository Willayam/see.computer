import Foundation

struct KeyLayout: Equatable {
    var centers = Array<CGPoint?>(repeating: nil, count: 30)
    var pitch: CGFloat = 1
    var reach: CGRect = .zero

    func costs(letter: Int, point: CGPoint?) -> [Double] {
        guard let center = centers[letter] else {
            var costs = Array(repeating: Double.infinity, count: centers.count)
            costs[letter] = 0
            return costs
        }
        let p = point ?? center
        let scale = 8 / (pitch * pitch)
        return centers.map {
            guard let center = $0 else { return .infinity }
            let x = center.x - p.x, y = center.y - p.y
            return (x*x+y*y)*scale
        }
    }
}

struct TapDecoder {
    struct State {
        var node: Int
        var cost: Double
        var priority: Double
    }
    let lexicon: Lexicon

    func candidates(costs: [[Double]]) -> [(Int, Double)] {
        var layer = [State(node: 0, cost: 0, priority: 0)]
        for row in costs {
            var next: [State] = []
            var best = Double.infinity
            for state in layer {
                for child in lexicon.children(state.node) {
                    let distance = row[lexicon.letter(child)]
                    if distance > 14 { continue }
                    let cost = state.cost + distance
                    let priority = cost - lexicon.best(child)
                    best = min(priority,best)
                    next.append(State(node: child, cost: cost, priority: priority))
                }
            }
            layer = next.filter { $0.priority <= best + 14 }
            if layer.count > 160 {
                layer.sort { $0.priority < $1.priority }
                layer.removeLast(layer.count-160)
            }
            if layer.isEmpty { break }
        }
        return layer.compactMap { state in lexicon.terminal(state.node).map { ($0, state.cost) } }
    }
}
