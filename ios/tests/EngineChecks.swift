import Foundation

@main
enum EngineChecks {
    static func main() {
        let engine = Engine.shared
        engine.start(previews: true)
        previewsKeepTheirTake(engine)
        print("Engine checks passed")
    }

    /// The engine can finish a preview for a cancelled take after the next take began.
    static func previewsKeepTheirTake(_ engine: Engine) {
        var heard: [(take: UInt64, words: String)] = []
        engine.onPreview = { heard.append(($0, $1)) }
        let field = UUID()
        func tap() -> DictationStart {
            let target = DictationTarget(field: field, before: nil, after: nil, selected: nil, hasText: false)
            return DictationStart(id: UUID(), asked: Date(), target: target)
        }

        let cancelled = LiveTake(start: tap(), utterance: engine.begin())
        let current = LiveTake(start: tap(), utterance: engine.begin())
        precondition(cancelled.utterance != 0 && current.utterance != cancelled.utterance)
        stub_emit(Int32(SeeEventPreview), cancelled.utterance, 0, "words from the cancelled take")
        stub_emit(Int32(SeeEventPreview), current.utterance, 0, "words from this take")
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))

        precondition(heard.map(\.take) == [cancelled.utterance, current.utterance], "A preview arrives with its take's number")
        let shown = heard.compactMap { current.preview($0.words, from: $0.take) }
        precondition(shown.map(\.words) == ["words from this take"], "Only this take's words show")
        precondition(shown.first?.startID == current.start.id)
    }
}
