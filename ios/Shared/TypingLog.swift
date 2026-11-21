import Foundation

/// Every key, swipe, suggestion and delete the keyboard handles, with the text around the
/// caret before and after, one JSON object per line in `Library/typing-log.jsonl` in the
/// App Group folder. Replaying it shows which words came out wrong and why. It never
/// leaves the phone. `ios/scripts/pull-typing-log.sh` copies it to a Mac.
enum TypingLog {
    static let key = "typingLog"
    static let store = UserDefaults(suiteName: Bridge.group)

    static var isOn: Bool { store?.object(forKey: key) as? Bool ?? true }

    /// About a month of heavy typing. One older file is kept when this one fills.
    private static let cap: UInt64 = 24 << 20
    private static let queue = DispatchQueue(label: "computer.see.typing-log", qos: .utility)

    private static let library: URL? = FileManager.default
        .containerURL(forSecurityApplicationGroupIdentifier: Bridge.group)?
        .appendingPathComponent("Library")
    private static var current: URL? { library?.appendingPathComponent("typing-log.jsonl") }
    private static var previous: URL? { library?.appendingPathComponent("typing-log.1.jsonl") }

    static func write(_ event: String, _ fields: [String: Any]) {
        guard isOn else { return }
        var object = fields
        object["e"] = event
        object["t"] = Int64(Date().timeIntervalSince1970 * 1000)
        guard JSONSerialization.isValidJSONObject(object),
              var line = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else { return }
        line.append(0x0A)
        queue.async {
            guard let library, let current, let previous else { return }
            try? FileManager.default.createDirectory(at: library, withIntermediateDirectories: true)
            if let size = try? FileManager.default.attributesOfItem(atPath: current.path)[.size] as? UInt64, size > cap {
                try? FileManager.default.removeItem(at: previous)
                try? FileManager.default.moveItem(at: current, to: previous)
            }
            if !FileManager.default.fileExists(atPath: current.path) {
                FileManager.default.createFile(atPath: current.path, contents: nil)
            }
            guard let handle = try? FileHandle(forWritingTo: current) else { return }
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: line)
        }
    }

    static func erase() {
        queue.async {
            for url in [current, previous].compactMap({ $0 }) { try? FileManager.default.removeItem(at: url) }
        }
    }
}
