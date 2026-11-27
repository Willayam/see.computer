import Foundation

@main
enum EngineChecks {
    static func main() {
        let engine = Engine.shared
        engine.start(previews: true)
        previewsKeepTheirTake(engine)
        engineHasOneCaller(engine)
        keptAudioComesFirst()
        nothingIsFedAfterTheEnd()
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

    /// The audio thread is feeding a block when the main thread finishes the take.
    static func engineHasOneCaller(_ engine: Engine) {
        engine.begin()
        stub_hold_next_feed()
        Thread { engine.feed([0.1]) }.start()
        stub_wait_until_feed_is_inside()

        let finished = DispatchSemaphore(value: 0)
        Thread {
            _ = engine.finish()
            finished.signal()
        }.start()
        precondition(finished.wait(timeout: .now() + 0.2) == .timedOut, "Finish waits for the feed inside the engine")
        precondition(stub_finishes() == 0)
        stub_release_feed()
        finished.wait()
        precondition(stub_finishes() == 1 && stub_overlaps() == 0, "The engine never has two callers")
    }

    /// The audio thread delivers a block while the take is still being handed its kept audio.
    static func keptAudioComesFirst() {
        let gate = CaptureGate(keeping: 3)
        gate.deliver([1, 2])
        gate.deliver([3, 4])
        let fed = Log<[Float]>()
        let handing = DispatchSemaphore(value: 0), proceed = DispatchSemaphore(value: 0)
        Thread {
            gate.begin { samples in
                if fed.add(samples) == 1 {
                    handing.signal()
                    proceed.wait()
                }
            }
        }.start()
        handing.wait()

        let delivered = DispatchSemaphore(value: 0)
        Thread {
            gate.deliver([5])
            delivered.signal()
        }.start()
        precondition(delivered.wait(timeout: .now() + 0.2) == .timedOut, "A new block waits for the kept audio")
        proceed.signal()
        delivered.wait()
        precondition(fed.entries == [[2, 3, 4], [5]], "The take hears the last of the kept audio, then the new block")

        gate.end()
        gate.deliver([6])
        let replayed = Log<[Float]>()
        gate.begin(replayingKept: false) { replayed.add($0) }
        gate.deliver([7])
        precondition(replayed.entries == [[7]], "A source that starts over replays nothing")
    }

    /// The take ends while the audio thread is handing over a block.
    static func nothingIsFedAfterTheEnd() {
        let gate = CaptureGate(keeping: 3)
        let order = Log<String>()
        let handing = DispatchSemaphore(value: 0), proceed = DispatchSemaphore(value: 0)
        gate.begin { _ in
            order.add("feed")
            handing.signal()
            proceed.wait()
        }
        Thread { gate.deliver([1]) }.start()
        handing.wait()

        let ended = DispatchSemaphore(value: 0)
        Thread {
            gate.end { order.add("end") }
            order.add("finish")
            ended.signal()
        }.start()
        precondition(ended.wait(timeout: .now() + 0.2) == .timedOut, "End waits for the block being handed over")
        proceed.signal()
        ended.wait()

        gate.begin { _ in order.add("next take") }
        gate.deliver([2])
        precondition(order.entries == ["feed", "end", "finish", "next take"], "No block reaches a take that has ended")
    }
}

/// What the checks observed, appended from more than one thread.
final class Log<Entry> {
    private let lock = NSLock()
    private var all: [Entry] = []

    @discardableResult
    func add(_ entry: Entry) -> Int {
        lock.lock()
        defer { lock.unlock() }
        all.append(entry)
        return all.count
    }

    var entries: [Entry] {
        lock.lock()
        defer { lock.unlock() }
        return all
    }
}
