import Foundation

enum Paths {
    static let documents = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
}

@main
enum HistoryChecks {
    static func main() throws {
        let zone = CommandLine.arguments[2]
        NSTimeZone.default = TimeZone(identifier: zone)!
        let first = ISO8601DateFormatter().date(from: "2026-10-25T00:15:00Z")!
        let second = first.addingTimeInterval(3600)
        let file = Paths.documents.appendingPathComponent("2026-10-25.md")
        if zone == "Europe/Stockholm" {
            try Data("## 02:15:00\n\nfirst\n\n## 02:15:00\n\nsecond\n\n".utf8).write(to: file)
            precondition(History.recent(limit: 2).map(\.at) == [second, first])
            try FileManager.default.removeItem(at: file)
        }
        History.append("first", at: first)
        History.append("second", at: second)
        History.append("same second", at: second)
        let stored = try String(contentsOf: file, encoding: .utf8)
        let offsets = zone == "UTC" ? ["+00:00"] : ["+02:00", "+01:00"]
        precondition(offsets.allSatisfy { stored.contains($0) })
        let recent = History.recent(limit: 3)
        precondition(recent.map(\.at) == [second, second, first])
        precondition(Set(recent.map(\.id)).count == 3)
        print("History dates and row identities passed in \(zone)")
    }
}
