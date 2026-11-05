import Foundation

/// One markdown file per day in the app's Documents folder, which the Files app
/// shows. The format matches the Mac's history files.
enum History {
    struct Entry: Identifiable, Equatable {
        let at: Date
        let text: String
        var id: Date { at }
    }

    private static let day: DateFormatter = formatter("yyyy-MM-dd")
    private static let time: DateFormatter = formatter("HH:mm:ss")
    private static let stamp: DateFormatter = formatter("yyyy-MM-dd HH:mm:ss")

    private static func formatter(_ format: String) -> DateFormatter {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = format
        return formatter
    }

    static func append(_ text: String, at date: Date) {
        let url = Paths.documents.appendingPathComponent("\(day.string(from: date)).md")
        let entry = Data("## \(time.string(from: date))\n\n\(text)\n\n".utf8)
        guard let handle = try? FileHandle(forWritingTo: url) else {
            try? entry.write(to: url)
            return
        }
        handle.seekToEndOfFile()
        handle.write(entry)
        try? handle.close()
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
        ("\n" + content).components(separatedBy: "\n## ").dropFirst().compactMap { block in
            let parts = block.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            guard let heading = parts.first, parts.count == 2,
                let at = stamp.date(from: "\(date) \(heading)")
            else { return nil }
            let text = parts[1].trimmingCharacters(in: .whitespacesAndNewlines)
            return text.isEmpty ? nil : Entry(at: at, text: text)
        }
    }
}
