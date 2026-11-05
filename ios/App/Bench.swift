#if DEBUG
    import AVFoundation
    import UIKit

    /// `ios/scripts/bench-phone.sh` launches the app with `-seeBench`. It
    /// transcribes `Documents/bench.wav` every few seconds and appends one JSON
    /// line per run to `Documents/bench.jsonl`, naming whether the app was in
    /// front, so the same run measures both once something else is opened.
    /// Silence playback keeps the app alive in the background without needing
    /// the microphone permission.
    final class Bench {
        private let player = AVAudioPlayerNode()
        private let audio = AVAudioEngine()
        private var samples: [Float] = []
        private var startedAt = Date()
        private var runs = 0
        private let limit = 40
        private let log = Paths.documents.appendingPathComponent("bench.jsonl")

        static var isRequested: Bool { CommandLine.arguments.contains("-seeBench") }

        func start() {
            samples = WavFile.samples16k(Paths.documents.appendingPathComponent("bench.wav")) ?? []
            try? FileManager.default.removeItem(at: log)
            write(["event": "launch", "samples": samples.count, "footprint_mb": footprint()])
            keepAlive()
            let engineStarted = Date()
            Engine.shared.onStage = { [weak self] stage in
                guard let self else { return }
                switch stage {
                case .ready:
                    self.write([
                        "event": "ready", "load_ms": ms(since: engineStarted),
                        "footprint_mb": self.footprint(), "peak_mb": self.peak(),
                    ])
                    self.run()
                case .failed(let message):
                    self.write(["event": "failed", "message": message])
                default:
                    break
                }
            }
            Engine.shared.onDone = { [weak self] _, result in
                guard let self else { return }
                var line: [String: Any] = [
                    "event": "run", "n": self.runs, "wall_ms": ms(since: self.startedAt),
                    "state": Bench.stateName, "footprint_mb": self.footprint(),
                    "audio_ms": self.samples.count / 16,
                ]
                switch result {
                case .success(let text): line["text"] = text
                case .failure(let failure): line["error"] = failure.message
                }
                self.write(line)
                self.runs += 1
                guard self.runs < self.limit else { return }
                DispatchQueue.main.asyncAfter(deadline: .now() + 2) { self.run() }
            }
            Engine.shared.start(previews: false)
        }

        private func run() {
            startedAt = Date()
            Engine.shared.begin()
            Engine.shared.feed(samples)
            _ = Engine.shared.finish()
        }

        private func keepAlive() {
            let session = AVAudioSession.sharedInstance()
            try? session.setCategory(.playback, options: [.mixWithOthers])
            try? session.setActive(true)
            audio.attach(player)
            let format = audio.outputNode.inputFormat(forBus: 0)
            audio.connect(player, to: audio.mainMixerNode, format: format)
            guard let silence = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4800) else { return }
            silence.frameLength = 4800
            try? audio.start()
            player.scheduleBuffer(silence, at: nil, options: .loops)
            player.play()
        }

        private static var stateName: String {
            switch UIApplication.shared.applicationState {
            case .active: return "foreground"
            case .inactive: return "inactive"
            case .background: return "background"
            @unknown default: return "unknown"
            }
        }

        private func footprint() -> Int {
            vmInfo().map { Int($0.phys_footprint / 1_048_576) } ?? -1
        }

        /// The highest footprint so far, so the model's first-run rewrite shows up.
        private func peak() -> Int {
            vmInfo().map { Int($0.ledger_phys_footprint_peak / 1_048_576) } ?? -1
        }

        private func vmInfo() -> task_vm_info_data_t? {
            var info = task_vm_info_data_t()
            var count = mach_msg_type_number_t(
                MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
            let status = withUnsafeMutablePointer(to: &info) { pointer in
                pointer.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                    task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
                }
            }
            return status == KERN_SUCCESS ? info : nil
        }

        private func write(_ line: [String: Any]) {
            guard var data = try? JSONSerialization.data(withJSONObject: line) else { return }
            data.append(0x0A)
            if let handle = try? FileHandle(forWritingTo: log) {
                handle.seekToEndOfFile()
                handle.write(data)
                try? handle.close()
            } else {
                try? data.write(to: log)
            }
        }
    }

    private func ms(since date: Date) -> Int {
        Int(Date().timeIntervalSince(date) * 1000)
    }
#endif
