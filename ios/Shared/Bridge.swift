import Foundation

/// What the app tells the keyboard. Only the app writes it.
struct SessionState: Codable, Equatable {
    enum Phase: String, Codable {
        case off, armed, recording, transcribing
    }

    var phase: Phase = .off
    var take: Take?
    var problem: String?
    /// When the take being recorded started, for the keyboard's clock.
    var since: Date?
}

struct Take: Codable, Equatable {
    var id: Int64
    var text: String
    var finished: Date
    /// The field was sent with the live words in it before this was written, so
    /// typing it would add the message again.
    var sent: Bool?
}

/// What the keyboard leaves behind when it has to open the app. Only the
/// keyboard writes it.
struct Handoff: Codable {
    var asked: Date
    var host: String?
}

/// The take so far, as it stands in the text field. Saved because the keyboard can
/// go away mid-take, and it has to replace these words when it comes back. Only the
/// keyboard writes it.
struct Draft: Codable {
    /// What the field held before the words, for telling whether they are still marked.
    var before: String?
    var spacing: String
    var words: String
    /// The user cancelled or sent the take, so its words must go. Any other end
    /// without a transcript, such as the app being killed, leaves them in the field.
    var dropped: Bool?

    var text: String { spacing + words }
}

/// Darwin notifications carry no payload. The receiver reads `state.json`, or
/// `words.json` for `words`.
enum Signal: String {
    /// `away`: the keyboard hid mid-take with the words still in the field.
    /// `sent`: the field emptied with the words in it, so the user sent them.
    case start, stop, cancel, ping, away, sent
    case changed, pong, words

    fileprivate var name: CFString { "computer.see.ios.\(rawValue)" as CFString }
}

enum Bridge {
    static let group = "group.computer.see.ios"
    static let armURL = URL(string: "seecomputer://arm")!
    /// A mic tap that opened the app still counts when the user comes back within this.
    static let handoffWindow: TimeInterval = 15
    /// A finished take older than this is left in History, not typed into whatever is focused now.
    static let insertWindow: TimeInterval = 10
    /// A take that ended while the keyboard was away still lands in the field it
    /// started in, when the user comes back within this.
    static let returnWindow: TimeInterval = 10 * 60

    private static var folder: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
    }

    static func read<Value: Decodable>(_ type: Value.Type, _ name: String) -> Value? {
        guard let url = folder?.appendingPathComponent(name),
            let data = try? Data(contentsOf: url)
        else { return nil }
        return try? JSONDecoder().decode(type, from: data)
    }

    static func write<Value: Encodable>(_ value: Value, _ name: String) {
        guard let url = folder?.appendingPathComponent(name),
            let data = try? JSONEncoder().encode(value)
        else { return }
        try? data.write(to: url, options: .atomic)
    }

    static func remove(_ name: String) {
        guard let url = folder?.appendingPathComponent(name) else { return }
        try? FileManager.default.removeItem(at: url)
    }

    // The meter runs at display rate, so it reads one byte rather than a JSON file.
    private static let levelDescriptor: Int32 = {
        guard let path = folder?.appendingPathComponent("level").path else { return -1 }
        return open(path, O_RDWR | O_CREAT, 0o644)
    }()

    static func writeLevel(_ level: Float) {
        var byte = UInt8(max(0, min(1, level)) * 255)
        _ = pwrite(levelDescriptor, &byte, 1, 0)
    }

    static func readLevel() -> Float {
        var byte: UInt8 = 0
        return pread(levelDescriptor, &byte, 1, 0) == 1 ? Float(byte) / 255 : 0
    }
}

enum Signals {
    private static var handlers: [String: () -> Void] = [:]
    private static var center: CFNotificationCenter { CFNotificationCenterGetDarwinNotifyCenter() }

    static func post(_ signal: Signal) {
        CFNotificationCenterPostNotification(
            center, CFNotificationName(signal.name), nil, nil, true)
    }

    /// Call on the main thread. The handler runs on the main thread.
    static func observe(_ signal: Signal, _ handler: @escaping () -> Void) {
        handlers[signal.name as String] = handler
        CFNotificationCenterAddObserver(
            center, nil,
            { _, _, name, _, _ in
                guard let name else { return }
                Signals.handlers[name.rawValue as String]?()
            },
            signal.name, nil, .deliverImmediately)
    }
}
