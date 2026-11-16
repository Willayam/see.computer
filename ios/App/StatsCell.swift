import SwiftUI

struct StatsCell: View {
    @ObservedObject var store: StatsStore
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var fraction = 0.0

    var body: some View {
        StatsPanel(value: store.value, fraction: fraction)
            .task(id: store.value) {
                fraction = reduceMotion ? 1 : 0
                guard !reduceMotion else { return }
                await Task.yield()
                guard !Task.isCancelled else { return }
                withAnimation(.timingCurve(0.2, 0.8, 0.2, 1, duration: 1.1)) { fraction = 1 }
            }
            .onChange(of: reduceMotion) { _, reduced in
                if reduced { withAnimation(nil) { fraction = 1 } }
            }
    }
}
