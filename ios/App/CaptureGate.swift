import os

/// Sends each block of audio to the take being recorded, or keeps the last of it for
/// the next take. The audio thread delivers blocks while the main thread begins and
/// ends takes, so one lock orders all three, and a block is handed over inside it. A
/// take then hears its kept audio before any new block, and nothing once `end` returns.
final class CaptureGate {
    private struct State {
        var take: (([Float]) -> Void)?
        var kept: [Float] = []
    }
    private let state = OSAllocatedUnfairLock(uncheckedState: State())
    private let limit: Int

    init(keeping samples: Int) {
        limit = samples
    }

    /// A source that starts over with each take has no kept audio to replay.
    func begin(replayingKept replay: Bool = true, _ take: @escaping ([Float]) -> Void) {
        state.withLockUnchecked { state in
            state.take = take
            if replay, !state.kept.isEmpty { take(state.kept) }
            state.kept = []
        }
    }

    /// `then` runs after the take's last block and before a block of the next take.
    func end(then: () -> Void = {}) {
        state.withLockUnchecked { state in
            state.take = nil
            then()
        }
    }

    func deliver(_ samples: [Float]) {
        guard !samples.isEmpty else { return }
        state.withLockUnchecked { state in
            if let take = state.take {
                take(samples)
                return
            }
            state.kept.append(contentsOf: samples)
            if state.kept.count > limit { state.kept.removeFirst(state.kept.count - limit) }
        }
    }

    func reset() {
        state.withLockUnchecked { $0 = State() }
    }
}
