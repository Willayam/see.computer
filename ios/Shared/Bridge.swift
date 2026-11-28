import CryptoKit
import Foundation

/// What the app tells the keyboard. Only the app writes it.
struct SessionState: Equatable {
    enum Phase: String, Codable {
        case off, armed, recording, transcribing
    }

    /// Whether the speech model can hear a take yet.
    enum Model: String, Codable {
        case loading, downloading, ready, failed
    }

    var phase: Phase = .off
    /// Finished takes the keyboard may still have to put in their fields, oldest first.
    /// Each has its own field to wait for, so a later take does not push it out.
    var takes: [Take] = []
    var problem: String?
    /// When the take being recorded started, for the keyboard's clock.
    var since: Date?
    var start: DictationStart?
    /// Missing from a file written before the app refused takes while the model loads.
    var model: Model?

    /// A take stays for as long as the keyboard could still place it.
    mutating func keep(_ take: Take, at now: Date) {
        dropExpired(at: now)
        takes.append(take)
    }

    mutating func dropExpired(at now: Date) {
        takes.removeAll { now.timeIntervalSince($0.finished) >= Bridge.returnWindow }
    }
}

extension SessionState: Codable {
    private enum CodingKeys: String, CodingKey {
        case phase, takes, take, problem, since, start, model
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        phase = try values.decode(Phase.self, forKey: .phase)
        // Until the app kept every waiting take, the file held the newest one alone.
        let newest = try values.decodeIfPresent(Take.self, forKey: .take)
        takes = try values.decodeIfPresent([Take].self, forKey: .takes) ?? newest.map { [$0] } ?? []
        problem = try values.decodeIfPresent(String.self, forKey: .problem)
        since = try values.decodeIfPresent(Date.self, forKey: .since)
        start = try values.decodeIfPresent(DictationStart.self, forKey: .start)
        model = try values.decodeIfPresent(Model.self, forKey: .model)
    }

    func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(phase, forKey: .phase)
        try values.encode(takes, forKey: .takes)
        try values.encodeIfPresent(problem, forKey: .problem)
        try values.encodeIfPresent(since, forKey: .since)
        try values.encodeIfPresent(start, forKey: .start)
        try values.encodeIfPresent(model, forKey: .model)
    }
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
        case waitForField, verifyTarget, offer, historyOnly
    }

    /// `home` is the identifier the take's field has now, when the keyboard has followed
    /// it there since the tap.
    func delivery(to field: UUID, at now: Date, hasDraft: Bool, home: UUID? = nil) -> Delivery {
        let age = now.timeIntervalSince(finished)
        guard sent != true, let start, age < Bridge.returnWindow else { return .historyOnly }
        if !hasDraft && age >= Bridge.insertWindow { return .offer }
        return (home ?? start.target.field) == field ? .verifyTarget : .waitForField
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

struct DictationPreview: Codable {
    let startID: UUID
    let words: String
}

/// A take while the engine hears it: the tap that started it and the number the engine
/// gave it. The engine can still be working out a preview for the take before, and that
/// preview carries the earlier number.
struct LiveTake {
    let start: DictationStart
    let utterance: UInt64

    func preview(_ words: String, from utterance: UInt64) -> DictationPreview? {
        utterance == self.utterance ? DictationPreview(startID: start.id, words: words) : nil
    }
}

/// What the keyboard leaves behind when it has to open the app. Only the
/// keyboard writes it.
struct Handoff: Codable {
    var asked: Date
    var host: String?
    var start: DictationStart?

    enum Return { case wait, start, ask, expired }

    func returning(to field: DictationTarget?, in bundle: String?, at now: Date = Date()) -> Return {
        guard let start, now.timeIntervalSince(asked) < Bridge.handoffWindow else { return .expired }
        guard let field else { return .wait }
        if let host, let bundle, host != bundle { return .wait }
        return start.target.matches(field) ? .start : .ask
    }
}

/// The take so far, as it stands in the text field. Saved because the keyboard can
/// go away mid-take, and it has to replace these words when it comes back. Only the
/// keyboard writes it.
struct Draft: Codable, Equatable {
    var spacing: String
    var words: String
    /// The user cancelled or sent the take, so its words must go. Any other end
    /// without a transcript, such as the app being killed, leaves them in the field.
    var dropped: Bool?
    var start: DictationStart?
    var committedBefore: Data?
    /// The host has shown these words. It committed them, or it held them as marked text
    /// for as long as a host takes to say that it shows none. Until then the draft is
    /// only what the keyboard asked for.
    var shown: Bool?
    /// The identifier of the field when the keyboard last put the draft there. iOS gives
    /// a field a new identifier each time the keyboard comes back to it.
    var field: UUID?

    var text: String { spacing + words }
    var home: UUID? { field ?? start?.target.field }

    /// Enough of a draft's own words to tell its field by them once the identifier has changed.
    private static let recognisable = 16

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

    /// Where the draft stands in the field. `before` is the text the host reports before
    /// the caret. It leaves marked text out, so an unchanged field still holds the mark
    /// this keyboard set, and a field that now ends in the draft has committed it.
    ///
    /// iOS can give the field a new identifier on return. In that case require the saved
    /// fingerprint of the preceding text and the complete draft before editing it.
    func placement(
        at current: DictationTarget?, before: String?, ownsMark: Bool, mayHaveMoved: Bool = false
    ) -> Placement? {
        guard let current, let target = start?.target, target.isReadable,
            target.selected == nil, current.selected == nil, target.after == current.after, current.hasText
        else { return nil }
        let known = home == current.field
        guard known || mayHaveMoved else { return nil }
        if let committedBefore, current.before == committedBefore, known || words.count >= Self.recognisable {
            return .committed
        }
        guard known else { return nil }
        if target.before == current.before { return ownsMark ? .marked : nil }
        return HostContext.ends(before, with: text) ? .committed : nil
    }

    enum Placement {
        case marked, committed
    }

    func canErase(before: String?) -> Bool {
        !text.isEmpty && before?.hasSuffix(text) == true
    }

    func recovery(at current: DictationTarget, before: String?) -> Ledger.Standing? {
        guard shown == true, let target = start?.target, current.selected == nil, target.after == current.after
        else { return nil }
        if HostContext.isEnd(before, of: text) { return .unconfirmed }
        return target.before != nil && target.before == current.before && current.hasText ? .offered : nil
    }

    /// The words that stay in the field as the user's own text once the keyboard stops
    /// updating the draft. Words the keyboard only asked the host to show do not count,
    /// or the final text would leave them out without their ever having been there.
    func kept(_ placement: Placement?, retyped: Bool) -> String {
        retyped || placement == .committed || shown == true ? words : ""
    }
}

/// The user edited the field mid-take. The words shown by then stay as the user's
/// text, and only the rest of the take lands at `target`. Only the keyboard writes it.
struct ManualEdit: Codable, Equatable {
    let startID: UUID?
    var target: DictationTarget?
    var kept: String? = nil

    func target(for start: DictationStart, at current: DictationTarget) -> DictationTarget? {
        guard startID == start.id, let target, target.selected == nil, target.matches(current) else { return nil }
        return target
    }
}

/// The text a host reports before the caret. A host reports the last sentence or two, at
/// most 300 characters. After the keyboard's own edit the keyboard holds its own guess,
/// which can be longer, until the host sends the field again.
enum HostContext {
    /// Enough of a text's end to tell it from a short word the field happens to share.
    private static let tail = 12

    static func ends(_ context: String?, with text: String) -> Bool {
        let context = form(context), text = form(text)
        guard !context.isEmpty, !text.isEmpty else { return false }
        let whole = text.drop(while: \.isWhitespace).count
        if context.hasSuffix(text) { return true }
        return text.hasSuffix(context) && context.count >= min(tail, whole)
    }

    /// Whether what the host reports, however little, is how `text` ends.
    static func isEnd(_ context: String?, of text: String) -> Bool {
        let context = form(context)
        return !context.isEmpty && form(text).hasSuffix(context)
    }

    /// The host has sent the field back as it was before `text` went in, so the text did
    /// not land. A field the host emptied meanwhile says nothing, and neither does a
    /// caret the user moved in front of the text.
    static func refused(_ text: String, before: String?, after: String?, now: String?, nowAfter: String?) -> Bool {
        let before = form(before), now = form(now)
        guard form(after) == form(nowAfter) else { return false }
        if now.isEmpty { return before.isEmpty }
        return before.hasSuffix(now) && !ends(now, with: text)
    }

    private static func form(_ text: String?) -> String {
        (text ?? "").precomposedStringWithCanonicalMapping
    }
}

/// What the keyboard has done about each take, by the tap that started it: the words it
/// put in the field while the take ran, the user's own edit, and how the finished take
/// was settled. Only the keyboard writes it. It outlives the keyboard process, so a take
/// goes into its field once however often iOS replaces the keyboard.
struct Ledger: Codable, Equatable {
    enum Standing: String, Codable {
        /// The insert went to the host, which has not had its say.
        case sent
        /// The keyboard could not place the take for sure. The user can insert it by hand.
        case offered
        case unconfirmed, copied
        case placed, dismissed
    }

    struct Entry: Codable, Equatable {
        let id: UUID
        var touched: Date
        var draft: Draft?
        var edit: ManualEdit?
        var standing: Standing?
        /// The identifier of the field the take was typed into, kept once the draft is gone.
        var field: UUID?

        /// The identifier the take's field has now, where the keyboard has put words or
        /// seen an edit there since the tap.
        var home: UUID? { field ?? draft?.home ?? edit?.target?.field }
    }

    private(set) var entries: [Entry] = []
    /// The newest take typed by a keyboard that kept one bookmark for all takes.
    var floor: Int64?

    /// Long after any take could still be placed, for words left in a field the user
    /// never came back to.
    private static let lifetime: TimeInterval = 24 * 60 * 60

    subscript(id: UUID) -> Entry? {
        entries.first { $0.id == id }
    }

    mutating func update(_ id: UUID, at now: Date = Date(), _ change: (inout Entry) -> Void) {
        let index = entries.firstIndex { $0.id == id } ?? entries.endIndex
        if index == entries.endIndex { entries.append(Entry(id: id, touched: now)) }
        entries[index].touched = now
        change(&entries[index])
    }

    mutating func remove(_ id: UUID) {
        entries.removeAll { $0.id == id }
    }

    mutating func reserveInsert(_ id: UUID, in field: UUID, from prior: Standing? = nil,
                                awaitingReply: Bool = false, save: (Ledger) -> Bool) -> Bool {
        guard self[id]?.standing == prior else { return false }
        var next = self
        next.update(id) {
            $0.field = field
            $0.standing = awaitingReply ? .sent : .unconfirmed
        }
        guard save(next) else { return false }
        self = next
        return true
    }

    /// Whether no other take has words in `field`. Words there that look like one take's
    /// draft can then only be that draft.
    func isFree(_ field: UUID, for id: UUID) -> Bool {
        !entries.contains { $0.id != id && $0.home == field }
    }

    /// Whether a field holds a take that an earlier keyboard sent without lasting to hear the
    /// host's answer. `before` is the text the host reports before the caret. Under a new
    /// identifier the field may still be the take's own, unless another take has words there.
    func shows(_ take: Take, in field: UUID, isOwn: Bool, before: String?, after: Data?) -> Bool {
        guard let start = take.start, let before else { return false }
        let entry = self[start.id]
        let sent = Transcript.rest(of: take.text, after: entry?.edit?.kept ?? "")
        if isOwn { return HostContext.ends(before.trimmingCharacters(in: .whitespaces), with: sent) }
        let origin = entry?.edit?.target ?? start.target
        guard !sent.isEmpty, isFree(field, for: start.id), after == origin.after else { return false }
        let context = before.hasSuffix(sent + " ") ? String(before.dropLast()) : before
        guard context.hasSuffix(sent) else { return false }
        let prefix = String(context.dropLast(sent.count))
        if origin.before == nil { return !origin.hasText && prefix.isEmpty }
        return DictationTarget.fingerprint(prefix) == origin.before ||
            (prefix.hasSuffix(" ") && DictationTarget.fingerprint(String(prefix.dropLast())) == origin.before)
    }

    /// A take without a tap comes from a build that named no field, so nothing can place it.
    func isSettled(_ take: Take) -> Bool {
        guard take.id > floor ?? 0, let id = take.start?.id else { return true }
        let standing = self[id]?.standing
        return standing == .placed || standing == .copied || standing == .dismissed
    }

    struct Offer: Equatable {
        enum Action { case insert, copy }
        let take: Take
        let words: String
        let action: Action
        let label: String
    }

    func offer(for take: Take, in field: UUID, at now: Date = Date()) -> Offer? {
        guard let start = take.start, take.sent != true, now.timeIntervalSince(take.finished) < Bridge.returnWindow,
              let entry = self[start.id] else { return nil }
        switch entry.standing {
        case .offered:
            let words = Transcript.rest(of: take.text, after: entry.edit?.kept ?? "")
            guard !words.isEmpty else { return nil }
            let label = start.target.field == field ? "Insert “\(words)”" : "Insert dictation"
            return Offer(take: take, words: words, action: .insert, label: label)
        case .unconfirmed:
            return Offer(take: take, words: take.text, action: .copy, label: "Copy dictation")
        default:
            return nil
        }
    }

    /// What a keyboard with one draft, one edit and one bookmark left behind.
    init(inserted: Int64? = nil, draft: Draft? = nil, edit: ManualEdit? = nil, at now: Date = Date()) {
        floor = inserted
        if let draft, let id = draft.start?.id { update(id, at: now) { $0.draft = draft } }
        if let edit, let id = edit.startID { update(id, at: now) { $0.edit = edit } }
    }

    static func read(in folder: URL? = Bridge.folder) -> Ledger? {
        do { return try load(in: folder) } catch { return nil }
    }

    private static func load(in folder: URL?) throws -> Ledger {
        if let ledger = try Bridge.load(Ledger.self, "ledger.json", in: folder) { return ledger }
        let old = ["inserted.json", "draft.json", "manual-edit.json"]
        let ledger = Ledger(
            inserted: try Bridge.load(Int64.self, old[0], in: folder),
            draft: try Bridge.load(Draft.self, old[1], in: folder),
            edit: try Bridge.load(ManualEdit.self, old[2], in: folder))
        if ledger.floor != nil || !ledger.entries.isEmpty {
            try ledger.store(in: folder)
            old.forEach { Bridge.remove($0, in: folder) }
        }
        return ledger
    }

    func store(in folder: URL? = Bridge.folder, at now: Date = Date()) throws {
        var kept = self
        kept.entries.removeAll { now.timeIntervalSince($0.touched) > Self.lifetime }
        try Bridge.store(kept, "ledger.json", in: folder)
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

    fileprivate static var folder: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
    }

    static func read<Value: Decodable>(_ type: Value.Type, _ name: String) -> Value? {
        try? load(type, name, in: folder)
    }

    fileprivate static func load<Value: Decodable>(_ type: Value.Type, _ name: String, in folder: URL?) throws -> Value? {
        guard let url = folder?.appendingPathComponent(name) else { throw CocoaError(.fileReadNoPermission) }
        do { return try JSONDecoder().decode(type, from: Data(contentsOf: url)) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { return nil }
    }

    fileprivate static func store<Value: Encodable>(_ value: Value, _ name: String, in folder: URL?) throws {
        guard let url = folder?.appendingPathComponent(name) else { throw CocoaError(.fileWriteNoPermission) }
        try JSONEncoder().encode(value).write(to: url, options: .atomic)
    }

    @discardableResult
    static func write<Value: Encodable>(_ value: Value, _ name: String) -> Bool {
        do { try store(value, name, in: folder); return true } catch { return false }
    }

    static func remove(_ name: String, in folder: URL? = Bridge.folder) {
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
