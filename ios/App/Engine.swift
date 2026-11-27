import Foundation
import os

/// The Rust engine behind `see_core.h`. Started once. Events arrive on the main thread.
final class Engine {
    enum Stage: Equatable {
        case waiting
        case downloading(percent: Int)
        case loading
        case ready
        case failed(String)
    }

    static let shared = Engine()

    var onStage: ((Stage) -> Void)?
    var onDone: ((UInt64, Result<String, EngineFailure>) -> Void)?
    /// The number `begin` gave the take, then its words so far.
    var onPreview: ((UInt64, String) -> Void)?

    /// The audio thread feeds the worker while the main thread begins and finishes
    /// takes. Its calls must not overlap, so every one of them goes through this lock.
    private let handle = OSAllocatedUnfairLock<OpaquePointer?>(uncheckedState: nil)

    static var isModelPrepared: Bool {
        let prepared = Paths.models.deletingLastPathComponent().appendingPathComponent("int8-prepared/ready")
        return FileManager.default.fileExists(atPath: prepared.path)
    }

    func start(previews: Bool) {
        let context = Unmanaged.passUnretained(self).toOpaque()
        handle.withLockUnchecked { handle in
            guard handle == nil else { return }
            handle = see_engine_start(
                Paths.models.path, Paths.dictionary.path, previews, context
            ) { context, kind, a, b, text in
                let engine = Unmanaged<Engine>.fromOpaque(context!).takeUnretainedValue()
                let text = text.map { String(cString: $0) } ?? ""
                DispatchQueue.main.async { engine.receive(Int(kind), a, b, text) }
            }
        }
    }

    /// After a failure the old worker has exited, so a new one takes its place.
    func restart(previews: Bool) {
        handle.withLockUnchecked { $0 = nil }
        start(previews: previews)
    }

    @discardableResult
    private func call<Answer>(_ worker: (OpaquePointer) -> Answer) -> Answer? {
        handle.withLockUnchecked { $0.map(worker) }
    }

    private func receive(_ kind: Int, _ a: UInt64, _ b: UInt64, _ text: String) {
        switch kind {
        case Int(SeeEventDownloading):
            onStage?(.downloading(percent: b == 0 ? 0 : Int(a * 100 / b)))
        case Int(SeeEventLoading), Int(SeeEventWarming):
            onStage?(.loading)
        case Int(SeeEventReady):
            onStage?(.ready)
        case Int(SeeEventFailed):
            onStage?(.failed(text))
        case Int(SeeEventPreview):
            onPreview?(a, text)
        case Int(SeeEventDone):
            onDone?(a, b == 0 ? .success(text) : .failure(EngineFailure(message: text)))
        default:
            break
        }
    }

    /// Starts a take and returns the number its previews carry. A preview still in the
    /// engine when the next take begins keeps the old number.
    @discardableResult
    func begin() -> UInt64 {
        call { see_engine_begin($0) } ?? 0
    }

    func feed(_ samples: [Float]) {
        call { handle in samples.withUnsafeBufferPointer { see_engine_feed(handle, $0.baseAddress, $0.count) } }
    }

    func finish() -> UInt64 {
        call { see_engine_finish($0, nil, 0) } ?? 0
    }

    func discard() {
        call { see_engine_discard($0) }
    }

    func warm() {
        call { see_engine_warm($0) }
    }
}

struct EngineFailure: Error {
    let message: String
}

enum Paths {
    private static var support: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    }

    static var speedLog: URL { support.appendingPathComponent("speed.tsv") }

    static var documents: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    /// The same layout under the support folder as on the Mac, so a prepared
    /// model copied over from one works here as it is.
    static var models: URL {
        support.appendingPathComponent("models/parakeet-tdt-0.6b-v3-onnx/int8", isDirectory: true)
    }

    /// `vocabulary.md` and `learned.md`, copied here from the connected folder.
    static var dictionary: URL {
        support.appendingPathComponent("dictionary", isDirectory: true)
    }
}
