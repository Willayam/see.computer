import Darwin
import Foundation

/// One markdown file per day in the app's Documents folder, which the Files app
/// shows. The format matches the Mac's history files.
enum History {
    struct Entry: Identifiable, Equatable {
        struct ID: Hashable {
            let at: Date
            let occurrence: Int
        }
        let at: Date
        let text: String
        var occurrence = 0
        var id: ID { ID(at: at, occurrence: occurrence) }
    }

    private static let day: DateFormatter = formatter("yyyy-MM-dd")
    private static let time = formatter("HH:mm:ss xxxxx")
    private static let offsetStamp = formatter("yyyy-MM-dd HH:mm:ss XXXXX")
    private static let stamp: DateFormatter = formatter("yyyy-MM-dd HH:mm:ss")

    private static func formatter(_ format: String) -> DateFormatter {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = format
        return formatter
    }

    @discardableResult
    static func append(_ text: String, at date: Date) -> Bool {
        let url = Paths.documents.appendingPathComponent("\(day.string(from: date)).md")
        let entry = Data("## \(time.string(from: date))\n\n\(text)\n\n".utf8)
        let fd = Darwin.open(url.path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0o666)
        guard fd >= 0 else { return false }
        let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        do {
            try handle.write(contentsOf: entry)
            try handle.close()
            return true
        } catch {
            return false
        }
    }

    static func recent(limit: Int) -> [Entry] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: Paths.documents.path)) ?? []
        let days = names.filter { $0.hasSuffix(".md") && day.date(from: String($0.dropLast(3))) != nil }
        var entries: [Entry] = []
        for name in days.sorted().reversed() {
            let url = Paths.documents.appendingPathComponent(name)
            guard let content = try? String(contentsOf: url, encoding: .utf8) else { continue }
            entries.append(contentsOf: parse(date: String(name.dropLast(3)), content).reversed())
            if entries.count >= limit { break }
        }
        return Array(entries.prefix(limit))
    }

    private static func parse(date: String, _ content: String) -> [Entry] {
        guard let start = day.date(from: date) else { return [] }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = stamp.timeZone
        guard let end = calendar.date(byAdding: .day, value: 1, to: start) else { return [] }
        let fold = TimeInterval(calendar.timeZone.secondsFromGMT(for: start.addingTimeInterval(-1))
            - calendar.timeZone.secondsFromGMT(for: end.addingTimeInterval(-1)))
        var previous: Date?
        var counts = [Date: Int]()
        return ("\n" + content).components(separatedBy: "\n## ").dropFirst().compactMap { block in
            let parts = block.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            guard let heading = parts.first, parts.count == 2 else { return nil }
            let local = "\(date) \(heading)"
            let instant = heading.contains(" ") ? offsetStamp.date(from: local) : legacyDate(local, fold, previous)
            guard let at = instant else { return nil }
            previous = at
            let text = parts[1].trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { return nil }
            let occurrence = counts[at, default: 0]
            counts[at] = occurrence + 1
            return Entry(at: at, text: text, occurrence: occurrence)
        }
    }

    private static func legacyDate(_ local: String, _ fold: TimeInterval, _ previous: Date?) -> Date? {
        guard let at = stamp.date(from: local) else { return nil }
        if fold > 0 {
            for delta in [-fold, fold] {
                let other = at.addingTimeInterval(delta)
                if stamp.string(from: other) == local {
                    let first = min(at, other)
                    return previous.map { $0 >= first } == true ? max(at, other) : first
                }
            }
        }
        return at
    }
}
