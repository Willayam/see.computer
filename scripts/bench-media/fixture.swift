import AppKit
import AVFoundation
import ScreenCaptureKit
import VideoToolbox
import Vision
import UniformTypeIdentifiers

// Synthetic content only. No microphone or other application windows are captured.
let width = 2560
let height = 1440
let lines = [
    "see.computer / Recording compression test",
    "Local transcription. Share the recording when you choose.",
    "const upload = { enabled: true, chunkSeconds: 1 };",
    "const languages = [\"sv\", \"en\"];",
    "Hej! Det här är ett test av diktering på svenska.",
    "Keep Swedish and English exactly as spoken.",
    "Invoice total: 1,234.56 SEK | Discount: 20% | Build: v0.1.0",
    "export async function uploadChunk(file: Blob): Promise<void> {",
    "  const response = await fetch(url, { method: \"PUT\", body: file });",
    "  if (!response.ok) throw new Error(\"Upload failed\");",
    "}",
    "Small text should stay readable while the page scrolls.",
    "Screenshots preserve details. Video provides the sequence.",
    "Recording 001   Uploading   Transcript pending",
    "Path: src-tauri/src/recorder.rs   Line: 42   Column: 18",
    "Queue: 3 files   Memory buffer: 8 MB   Retry: 2 seconds",
    "A paused network must not interrupt local recording.",
    "Agent instructions: inspect the screenshot at 00:04.",
    "Symbols: => != === && || [] {} < > / _ + % @ #",
    "Swedish letters: å ä ö Å Ä Ö. Numbers: 0 1 2 3 4 5 6 7 8 9"
]

func drawPage(_ context: CGContext, size: CGSize, offset: CGFloat = 0) {
    context.setFillColor(CGColor(red: 0.96, green: 0.97, blue: 0.98, alpha: 1))
    context.fill(CGRect(origin: .zero, size: size))
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: true)
    for row in 0..<36 {
        let y = CGFloat(60 + row * 62) - offset
        let index = row % lines.count
        let fontSize: CGFloat = row == 0 ? 38 : [24, 28, 32][row % 3]
        let color = index == 2 || index == 3 ? NSColor.systemBlue : NSColor(calibratedWhite: 0.12, alpha: 1)
        (lines[index] as NSString).draw(at: NSPoint(x: 64, y: y), withAttributes: [
            .font: NSFont.monospacedSystemFont(ofSize: fontSize, weight: row == 0 ? .semibold : .regular),
            .foregroundColor: color
        ])
    }
    NSGraphicsContext.restoreGraphicsState()
}

func png(_ path: String) throws {
    let tall = 2400
    let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: tall,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    let context = NSGraphicsContext(bitmapImageRep: bitmap)!.cgContext
    context.translateBy(x: 0, y: CGFloat(tall))
    context.scaleBy(x: 1, y: -1)
    drawPage(context, size: CGSize(width: width, height: tall))
    try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))
}

func ocr(_ path: String) throws {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.recognitionLanguages = ["en-US", "sv-SE"]
    request.usesLanguageCorrection = false
    try VNImageRequestHandler(url: URL(fileURLWithPath: path)).perform([request])
    let result = request.results?.compactMap { $0.topCandidates(1).first?.string } ?? []
    let data = try JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys])
    FileHandle.standardOutput.write(data)
}

final class Page: NSView {
    var offset: CGFloat = 0
    var image: NSImage?
    override var isFlipped: Bool { true }
    override func draw(_ dirtyRect: NSRect) {
        image?.draw(in: NSRect(x: 0, y: -offset / 2, width: 1280, height: 1200),
            from: .zero, operation: .copy, fraction: 1, respectFlipped: true, hints: nil)
    }
}

// Frame state stays on queue. Writer callbacks serialize segment output; results are read after finishWriting.
final class Capture: NSObject, SCStreamOutput, SCStreamDelegate, AVAssetWriterDelegate, @unchecked Sendable {
    let writer: AVAssetWriter
    let input: AVAssetWriterInput
    var frames = 0
    var skipped = 0
    var firstPTS: CMTime?
    var lastPTS: CMTime?
    let segmentDirectory: URL?
    let started = Date()
    var segmentCount = 0
    var segmentEvents: [[String: Any]] = []
    let queue = DispatchQueue(label: "see-media-bench.capture", qos: .userInitiated)
    init(path: String, segmented: Bool) throws {
        segmentDirectory = segmented ? URL(fileURLWithPath: path) : nil
        if let directory = segmentDirectory {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            writer = AVAssetWriter(contentType: .mpeg4Movie)
            writer.outputFileTypeProfile = .mpeg4AppleHLS
            writer.preferredOutputSegmentInterval = CMTime(value: 1, timescale: 1)
        } else {
            writer = try AVAssetWriter(outputURL: URL(fileURLWithPath: path), fileType: .mov)
        }
        input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width, AVVideoHeightKey: height,
            AVVideoEncoderSpecificationKey: [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: true],
            AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: segmented ? 2_000_000 : 4_000_000,
                AVVideoMaxKeyFrameIntervalKey: segmented ? 15 : 30, AVVideoAllowFrameReorderingKey: false]
        ])
        input.expectsMediaDataInRealTime = true
        writer.add(input)
        super.init()
        if segmented { writer.delegate = self }
    }
    func assetWriter(_ writer: AVAssetWriter, didOutputSegmentData data: Data,
                     segmentType: AVAssetSegmentType, segmentReport: AVAssetSegmentReport?) {
        guard let directory = segmentDirectory else { return }
        let name = segmentType == .initialization ? "init.mp4" : String(format: "segment-%03d.m4s", segmentCount)
        do { try data.write(to: directory.appendingPathComponent(name), options: .atomic) }
        catch { fputs("Segment write failed: \(error)\n", stderr); writer.cancelWriting(); return }
        segmentEvents.append(["file": name, "bytes": data.count, "ready_ms": Date().timeIntervalSince(started) * 1000])
        if segmentType != .initialization { segmentCount += 1 }
    }
    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sample.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let status = attachments.first?[.status] as? Int,
              status == SCFrameStatus.complete.rawValue else { return }
        let pts = sample.presentationTimeStamp
        if firstPTS == nil {
            guard writer.status == .unknown else { return }
            if segmentDirectory != nil { writer.initialSegmentStartTime = .zero }
            guard writer.startWriting() else { return }
            writer.startSession(atSourceTime: .zero)
            firstPTS = pts
        }
        var timing = CMSampleTimingInfo(duration: sample.duration,
            presentationTimeStamp: CMTimeSubtract(pts, firstPTS!), decodeTimeStamp: .invalid)
        var rebased: CMSampleBuffer?
        guard CMSampleBufferCreateCopyWithNewTiming(allocator: kCFAllocatorDefault, sampleBuffer: sample,
            sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &rebased) == noErr,
            let rebased else { skipped += 1; return }
        if input.isReadyForMoreMediaData && input.append(rebased) { frames += 1; lastPTS = pts }
        else { skipped += 1 }
    }
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        fputs("Capture error: \(error)\n", stderr)
    }
}

@MainActor func capture(path: String, seconds: Double, segmented: Bool) async throws {
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    let page = Page(frame: NSRect(x: 0, y: 0, width: 1280, height: 720))
    page.image = NSImage(contentsOf: URL(fileURLWithPath: path).deletingLastPathComponent().appendingPathComponent("page.png"))
    let window = NSWindow(contentRect: page.frame, styleMask: [.titled], backing: .buffered, defer: false)
    window.title = "see.computer synthetic media benchmark"
    window.contentView = page
    window.center()
    window.orderFrontRegardless()
    try await Task.sleep(nanoseconds: 500_000_000)
    let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
    guard let target = content.windows.first(where: { $0.windowID == CGWindowID(window.windowNumber) }) else {
        throw NSError(domain: "benchmark", code: 1, userInfo: [NSLocalizedDescriptionKey: "Synthetic window unavailable"])
    }
    let recorder = try Capture(path: path, segmented: segmented)
    let config = SCStreamConfiguration()
    config.width = width; config.height = height
    config.minimumFrameInterval = CMTime(value: 1, timescale: segmented ? 15 : 30)
    config.queueDepth = 3
    config.showsCursor = false
    config.capturesAudio = false
    let stream = SCStream(filter: SCContentFilter(desktopIndependentWindow: target), configuration: config, delegate: recorder)
    try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: recorder.queue)
    try await stream.startCapture()
    let start = Date()
    while Date().timeIntervalSince(start) < seconds {
        let elapsed = Date().timeIntervalSince(start)
        page.offset = CGFloat(min(900, max(0, elapsed - 3) * 150))
        page.needsDisplay = true
        try await Task.sleep(nanoseconds: 33_333_333)
    }
    try await stream.stopCapture()
    await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
        recorder.queue.async {
            recorder.input.markAsFinished()
            recorder.writer.finishWriting { continuation.resume() }
        }
    }
    window.orderOut(nil)
    guard recorder.writer.status == .completed else { throw recorder.writer.error ?? NSError(domain: "benchmark", code: 2) }
    let result: [String: Any] = ["frames": recorder.frames, "skipped": recorder.skipped,
        "hardware_required": true, "capture": "ScreenCaptureKit, synthetic window only",
        "seconds": seconds, "width": width, "height": height, "segments": recorder.segmentEvents]
    FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: result))
}

let arguments = CommandLine.arguments
guard arguments.count >= 3 else { fatalError("fixture png|ocr|capture PATH [SECONDS]") }
if arguments[1] == "capture" || arguments[1] == "capture-segments" {
    Task { @MainActor in
        do { try await capture(path: arguments[2], seconds: arguments.count > 3 ? Double(arguments[3])! : 12,
                              segmented: arguments[1] == "capture-segments"); exit(0) }
        catch { fputs("\(error)\n", stderr); exit(1) }
    }
    NSApplication.shared.run()
} else {
    do {
        if arguments[1] == "png" { try png(arguments[2]) }
        else if arguments[1] == "ocr" { try ocr(arguments[2]) }
        else { fatalError("Unknown operation") }
    } catch { fputs("\(error)\n", stderr); exit(1) }
}
