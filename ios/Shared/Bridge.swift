import CryptoKit
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
    var start: DictationStart?
}

struct Take: Codable, Equatable {
    var id: Int64
    var text: String
    var finished: Date
    /// The field was sent with the live words in it before this was written, so
    /// typing it would add the message again.
    var sent: Bool?
    var start: DictationStart?

    enum Delivery {
        case waitForField, verifyTarget, historyOnly
    }

    func delivery(to field: UUID, at now: Date, hasDraft: Bool) -> Delivery {
        let window = hasDraft ? Bridge.returnWindow : Bridge.insertWindow
        guard sent != true, let start, now.timeIntervalSince(finished) < window else { return .historyOnly }
        return start.target.field == field ? .verifyTarget : .waitForField
    }
}

struct DictationTarget: Codable, Equatable {
    let field: UUID
    let before: Data?
    let after: Data?
    let selected: Data?
    let hasText: Bool

    init(field: UUID, before: String?, after: String?, selected: String?, hasText: Bool) {
        self.field = field
        self.before = before.map(Self.fingerprint)
        self.after = after.map(Self.fingerprint)
        self.selected = selected.flatMap { $0.isEmpty ? nil : Self.fingerprint($0) }
        self.hasText = hasText
    }

    var isReadable: Bool { !hasText || before != nil || after != nil || selected != nil }

    func matchesPosition(_ current: Self) -> Bool {
        isReadable && field == current.field && before == current.before
            && after == current.after && selected == current.selected
    }

    func matches(_ current: Self?) -> Bool {
        guard let current else { return false }
        return matchesPosition(current) && hasText == current.hasText
    }

    fileprivate static func fingerprint(_ text: String) -> Data {
        Data(SHA256.hash(data: Data(text.precomposedStringWithCanonicalMapping.utf8)))
    }
}

struct DictationStart: Codable, Equatable {
    let id: UUID
    let asked: Date
    let target: DictationTarget
}

struct StartRequest: Codable {
    let start: DictationStart
    let nearby: String
}

struct DictationPreview: Codable {
    let startID: UUID
    let words: String
}

/// What the keyboard leaves behind when it has to open the app. Only the
/// keyboard writes it.
struct Handoff: Codable {
    var asked: Date
    var host: String?
    var start: DictationStart?
    var nearby: String?
}

/// The take so far, as it stands in the text field. Saved because the keyboard can
/// go away mid-take, and it has to replace these words when it comes back. Only the
/// keyboard writes it.
struct Draft: Codable {
    var spacing: String
    var words: String
    /// The user cancelled or sent the take, so its words must go. Any other end
    /// without a transcript, such as the app being killed, leaves them in the field.
    var dropped: Bool?
    var start: DictationStart?
    var committedBefore: Data?

    var text: String { spacing + words }

    init(start: DictationStart, before: String?, spacing: String, words: String) {
        self.start = start
        self.spacing = spacing
        self.words = words
        let prefix = before ?? ""
        let committed = prefix + text
        if before.map(DictationTarget.fingerprint) == start.target.before,
            committed.count == prefix.count + text.count {
            committedBefore = DictationTarget.fingerprint(committed)
        }
    }

    func placement(at current: DictationTarget?, ownsMark: Bool) -> Placement? {
        guard let current, let target = start?.target, target.isReadable, target.field == current.field,
            target.selected == nil, current.selected == nil, target.after == current.after, current.hasText
        else { return nil }
        if let committedBefore, current.before == committedBefore {
            return .committed
        }
        if ownsMark, target.matchesPosition(current) { return .marked }
        return nil
    }

    enum Placement {
        case marked, committed
    }
}

/// The user edited the field mid-take. The words shown by then stay as the user's
/// text, and only the rest of the take lands at `target`. Only the keyboard writes it.
struct ManualEdit: Codable {
    let startID: UUID?
    var target: DictationTarget?
    var kept: String? = nil

    func target(for start: DictationStart, at current: DictationTarget) -> DictationTarget? {
        guard startID == start.id, let target, target.field == start.target.field,
            target.selected == nil, target.matches(current) else { return nil }
        return target
    }
}

enum Transcript {
    /// What `final` says after `kept`, an earlier pass over how it starts. Words match
    /// loosely, because the last pass can still change a few of them.
    static func rest(of final: String, after kept: String) -> String {
        let keptForms = kept.split(whereSeparator: \.isWhitespace).map(form)
        guard !keptForms.isEmpty else { return final }
        let words = final.split(whereSeparator: \.isWhitespace)
        let forms = words.prefix(keptForms.count + 32).map(form)
        let n = keptForms.count, m = forms.count
        guard m > 0 else { return "" }
        // Word edit distance from the kept words to each start of the final text. The
        // start that costs least is what they stood for.
        var row = (0...m).map(Double.init)
        for i in 1...n {
            var next = [Double(i)] + [Double](repeating: 0, count: m)
            for j in 1...m {
                next[j] = min(row[j] + 1, next[j - 1] + 1, row[j - 1] + cost(keptForms[i - 1], forms[j - 1]))
            }
            row = next
        }
        // On a tie, the start nearest the kept word count: a word heard differently stands for one word.
        let cut = (0...m).min { (row[$0], abs($0 - n)) < (row[$1], abs($1 - n)) }!
        return cut < words.count ? String(final[words[cut].startIndex...]) : ""
    }

    /// A word cut short or spelled a little differently is mostly the same word.
    private static func cost(_ a: String, _ b: String) -> Double {
        if a == b { return 0 }
        return a.hasPrefix(b) || b.hasPrefix(a) || (a.count > 1 && a.prefix(2) == b.prefix(2)) ? 0.5 : 1
    }

    private static func form(_ word: Substring) -> String {
        word.trimmingCharacters(in: .alphanumerics.inverted).lowercased()
    }
}

/// Darwin notifications carry no payload. The receiver reads `state.json`, or
/// `words.json` for `words`.
enum Signal: String {
    /// `away`: the keyboard hid mid-take with the words still in the field.
    /// `sent`: the field emptied with the words in it, so the user sent them.
    case start, stop, cancel, ping, away, sent
    case changed, pong, words, typingLanguagesChanged

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
