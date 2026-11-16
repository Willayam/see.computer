import Foundation
import WidgetKit

@MainActor
final class StatsStore: ObservableObject {
    @Published private(set) var value = StatsSnapshot.read() ?? .empty
    // An iCloud read can wait, so timing writes need their own queue.
    nonisolated private static let speedQueue = DispatchQueue(label: "computer.see.speed", qos: .utility)
    nonisolated private static let queue = DispatchQueue(label: "computer.see.stats", qos: .utility)

    func refresh() {
        Self.queue.async {
            do {
                let speed = try Self.speedQueue.sync {
                    try Self.prepareSpeedLog()
                    return try String(contentsOf: Paths.speedLog, encoding: .utf8)
                }
                let days = Self.readDays(in: Paths.documents) + DictionaryFolder.historyDays()
                let value = WordTally.compute(days: days, speedLines: speed, today: Date())
                if let url = StatsSnapshot.url {
                    try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
                    try JSONEncoder().encode(value).write(to: url, options: .atomic)
                    WidgetCenter.shared.reloadAllTimelines()
                }
                DispatchQueue.main.async { self.value = value }
            } catch {
                Diagnostics.log("stats_failed", ["error": error.localizedDescription])
            }
        }
    }

    nonisolated static func readDays(in folder: URL) -> [WordTally.Day] {
        let files = (try? FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil)) ?? []
        return files.compactMap { listed in
            // Older iCloud Drive shows a file not yet downloaded as ".name.icloud";
            // a coordinated read of the real name downloads it.
            var name = listed.lastPathComponent
            if name.hasPrefix("."), name.hasSuffix(".icloud") { name = String(name.dropFirst().dropLast(7)) }
            let url = folder.appendingPathComponent(name)
            guard WordTally.dayName(name) != nil else { return nil }
            var result: WordTally.Day?
            var failure: NSError?
            NSFileCoordinator().coordinate(readingItemAt: url, options: [], error: &failure) { coordinated in
                if let text = try? String(contentsOf: coordinated, encoding: .utf8) {
                    result = WordTally.Day(name: name, text: text)
                }
            }
            return result
        }
    }

    nonisolated static func logTake(at: Date, seconds: Double, text: String) {
        speedQueue.async {
            do {
                try prepareSpeedLog(before: at)
                guard seconds.isFinite, seconds >= 0, seconds * 1000 < Double(Int64.max) else { return }
                let row = "\(Int64(at.timeIntervalSince1970 * 1000))\t\(Int64(seconds * 1000))\t\(WordTally.words(text))\n"
                let handle = try FileHandle(forWritingTo: Paths.speedLog)
                defer { try? handle.close() }
                try handle.seekToEnd()
                try handle.write(contentsOf: Data(row.utf8))
            } catch {
                Diagnostics.log("speed_log_failed", ["error": error.localizedDescription])
            }
        }
    }

    nonisolated private static func prepareSpeedLog(before cutoff: Date = .distantFuture) throws {
        guard !FileManager.default.fileExists(atPath: Paths.speedLog.path) else { return }
        let diagnostics = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: Bridge.group)?
            .appendingPathComponent("Library/diagnostics.tsv")
        let text = diagnostics.flatMap { try? String(contentsOf: $0, encoding: .utf8) } ?? ""
        var seconds: Double?
        var rows = ""
        for line in text.split(separator: "\n") {
            let fields = line.split(separator: "\t", omittingEmptySubsequences: false)
            guard fields.count == 4, fields[1] == "app", let ms = Int64(fields[0]),
                  Double(ms) / 1000 < cutoff.timeIntervalSince1970 else { continue }
            let details = fields[3].split(separator: " ")
            if fields[2] == "take_stopped" {
                seconds = details.first { $0.hasPrefix("seconds=") }.flatMap { Double($0.dropFirst(8)) }
            } else if fields[2] == "transcript_delivered" {
                defer { seconds = nil }
                if let duration = seconds, duration.isFinite, duration >= 0, duration * 1000 < Double(Int64.max),
                   let words = details.first(where: { $0.hasPrefix("words=") }).flatMap({ UInt64($0.dropFirst(6)) }) {
                    rows += "\(ms)\t\(Int64(duration * 1000))\t\(words)\n"
                }
            }
        }
        try FileManager.default.createDirectory(at: Paths.speedLog.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data(rows.utf8).write(to: Paths.speedLog, options: .atomic)
    }
}
