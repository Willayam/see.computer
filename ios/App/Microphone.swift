import AVFoundation
import os

/// Owns the audio engine. While armed it keeps the last 300 ms, as the Mac does,
/// so a word started on the tap is not clipped. While capturing it hands every
/// buffer to the sink as 16 kHz mono floats.
final class Microphone {
    private var engine = AVAudioEngine()
    private let target = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!

    private let gate = CaptureGate(keeping: 16_000 * 3 / 10)
    /// Replaced on the main thread after a route change, read on the audio thread.
    private let converter = OSAllocatedUnfairLock<AVAudioConverter?>(uncheckedState: nil)

    func start() throws {
        if startTestInput() { return }
        guard !engine.isRunning else { return }
        let session = AVAudioSession.sharedInstance()
        // No `.allowBluetoothHFP`: it would move AirPods to the call profile and
        // ruin whatever they are playing, for as long as the mic stays armed.
        try session.setCategory(
            .playAndRecord, mode: .default,
            options: [.mixWithOthers, .allowBluetoothA2DP, .defaultToSpeaker])
        try session.setActive(true)
        try installTap()
        engine.prepare()
        try engine.start()
    }

    func stop() {
        stopTestInput()
        guard Microphone.testInput == nil else { return }
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        gate.reset()
        converter.withLockUnchecked { $0 = nil }
        Bridge.writeLevel(0)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    /// After a route change the engine has stopped and the input format may differ.
    func restart() throws {
        guard Microphone.testInput == nil else { return }
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        try installTap()
        engine.prepare()
        try engine.start()
    }

    /// Apple asks for new audio objects after the media services restart.
    func rebuild() {
        engine = AVAudioEngine()
    }

    func beginCapture(_ sink: @escaping ([Float]) -> Void) {
        let take: ([Float]) -> Void = { samples in
            sink(samples)
            let energy = samples.reduce(Float(0)) { $0 + $1 * $1 } / Float(samples.count)
            // From -50 dB, a quiet room, to -10 dB, loud speech close to the phone.
            Bridge.writeLevel((10 * log10(max(energy, 1e-9)) + 50) / 40)
        }
        if let input = Microphone.testInput {
            testQueue.sync {
                input.rewind()
                gate.begin(replayingKept: false, take)
            }
        } else {
            gate.begin(take)
        }
    }

    /// No block reaches the sink once this returns, so the caller can finish the take.
    func endCapture() {
        gate.end { Bridge.writeLevel(0) }
    }

    private func installTap() throws {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw MicrophoneFailure.noInput }
        let fresh = AVAudioConverter(from: format, to: target)
        converter.withLockUnchecked { $0 = fresh }
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 2048, format: format) { [weak self] buffer, _ in
            self?.receive(buffer)
        }
    }

    private func receive(_ buffer: AVAudioPCMBuffer) {
        gate.deliver(convert(buffer))
    }

    private func convert(_ buffer: AVAudioPCMBuffer) -> [Float] {
        guard let converter = converter.withLockUnchecked({ $0 }) else { return [] }
        let capacity = AVAudioFrameCount(
            Double(buffer.frameLength) * target.sampleRate / buffer.format.sampleRate + 64)
        guard let output = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else {
            return []
        }
        var served = false
        var error: NSError?
        converter.convert(to: output, error: &error) { _, status in
            if served {
                status.pointee = .noDataNow
                return nil
            }
            served = true
            status.pointee = .haveData
            return buffer
        }
        guard error == nil, let channel = output.floatChannelData?[0] else { return [] }
        return Array(UnsafeBufferPointer(start: channel, count: Int(output.frameLength)))
    }

    /// Debug builds replace the microphone when `Documents/test-input.wav`
    /// exists, as `SEE_COMPUTER_AUDIO_FILE` does on the Mac. It also keeps the input
    /// node out of the simulator, where it goes through the Mac's audio service and
    /// aborts the app if that does not answer in time.
    private static let testInput: TestInput? = {
        #if DEBUG
            return TestInput(url: Paths.documents.appendingPathComponent("test-input.wav"))
        #else
            return nil
        #endif
    }()
    private var testTimer: DispatchSourceTimer?
    private let testQueue = DispatchQueue(label: "computer.see.ios.file-mic", qos: .userInitiated)
    private let testSilence = AVAudioPlayerNode()

    private func startTestInput() -> Bool {
        guard let input = Microphone.testInput else { return false }
        guard testTimer == nil else { return true }
        let timer = DispatchSource.makeTimerSource(queue: testQueue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(100))
        timer.setEventHandler { [weak self] in self?.gate.deliver(input.next(count: 1600)) }
        timer.resume()
        testTimer = timer
        playTestSilence()
        return true
    }

    private func stopTestInput() {
        testTimer?.cancel()
        testTimer = nil
        guard Microphone.testInput != nil, engine.isRunning else { return }
        engine.stop()
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    /// The real microphone keeps the app running behind the host app. A file cannot, so
    /// silence plays instead, or iOS suspends the app a few seconds into a take.
    private func playTestSilence() {
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, options: [.mixWithOthers])
        try? session.setActive(true)
        if testSilence.engine !== engine {
            testSilence.engine?.detach(testSilence)
            engine.attach(testSilence)
        }
        let format = engine.outputNode.inputFormat(forBus: 0)
        engine.connect(testSilence, to: engine.mainMixerNode, format: format)
        guard let silence = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4800) else { return }
        silence.frameLength = 4800
        try? engine.start()
        testSilence.scheduleBuffer(silence, at: nil, options: .loops)
        testSilence.play()
    }
}

enum MicrophoneFailure: Error {
    case noInput
}

/// How long and how loud one take's audio was, for the diagnostics log. Fed from
/// the audio thread.
final class CaptureStats: Sendable {
    private struct Sums {
        var count = 0
        var peak: Float = 0
        var squares: Double = 0
    }
    private let sums = OSAllocatedUnfairLock(initialState: Sums())

    func add(_ samples: [Float]) {
        let peak = samples.reduce(Float(0)) { max($0, abs($1)) }
        let squares = samples.reduce(0.0) { $0 + Double($1 * $1) }
        sums.withLock {
            $0.count += samples.count
            $0.peak = max($0.peak, peak)
            $0.squares += squares
        }
    }

    /// Seconds of audio, and peak and RMS level in dBFS.
    func summary() -> (seconds: Double, peak: Double, rms: Double) {
        let sums = sums.withLock { $0 }
        let rms = sums.count == 0 ? 0 : (sums.squares / Double(sums.count)).squareRoot()
        let decibels = { (level: Double) in 20 * log10(max(level, 1e-9)) }
        return (Double(sums.count) / 16_000, decibels(Double(sums.peak)), decibels(rms))
    }
}

/// Plays a WAV file once per take at the pace the tap asks for, then silence.
final class TestInput {
    private let samples: [Float]
    private var position = 0

    init?(url: URL) {
        guard let samples = WavFile.samples16k(url), !samples.isEmpty else { return nil }
        self.samples = samples
    }

    func rewind() {
        position = 0
    }

    func next(count: Int) -> [Float] {
        let end = min(position + count, samples.count)
        let out = Array(samples[position..<end]) + [Float](repeating: 0, count: count - (end - position))
        position = end
        return out
    }
}

enum WavFile {
    static func samples16k(_ url: URL) -> [Float]? {
        guard let file = try? AVAudioFile(forReading: url),
            let target = AVAudioFormat(
                commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false),
            let converter = AVAudioConverter(from: file.processingFormat, to: target),
            let input = AVAudioPCMBuffer(
                pcmFormat: file.processingFormat, frameCapacity: AVAudioFrameCount(file.length)),
            (try? file.read(into: input)) != nil,
            let output = AVAudioPCMBuffer(
                pcmFormat: target,
                frameCapacity: AVAudioFrameCount(
                    Double(file.length) * 16_000 / file.processingFormat.sampleRate + 64))
        else { return nil }
        var served = false
        var error: NSError?
        converter.convert(to: output, error: &error) { _, status in
            if served {
                status.pointee = .endOfStream
                return nil
            }
            served = true
            status.pointee = .haveData
            return input
        }
        guard error == nil, let channel = output.floatChannelData?[0] else { return nil }
        return Array(UnsafeBufferPointer(start: channel, count: Int(output.frameLength)))
    }
}
