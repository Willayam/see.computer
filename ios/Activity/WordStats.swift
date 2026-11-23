import Foundation

struct WordStats: Codable, Hashable, Sendable {
    var total: Int
    var today: Int
    var firstDictationDay: String? = nil
    var wpm: Int?
    var todayWpm: Int?
    var timesTyping: Double?
    var hoursSaved: Double?
    var levelIndex: Int
    var levelName: String?
    var nextName: String?
    var toGo: Int
    var progress: Double

    static let empty = WordTally.compute(days: [], speedLines: "", today: Date())

    var savedMinutes: Double? {
        guard let wpm, wpm > 40 else { return nil }
        let minutes = Double(total) * Double(wpm - 40) / (40 * Double(wpm))
        return minutes >= 1 ? minutes : nil
    }

    func sinceLabel(today: Date = Date()) -> String {
        let formatter = WordTally.dayFormatter()
        guard let firstDictationDay, let first = formatter.date(from: firstDictationDay),
              let elapsed = formatter.calendar.dateComponents([.day], from: first, to: today).day,
              elapsed >= 7 else { return "Words dictated" }
        formatter.dateFormat = "d MMM"
        return "Dictated since \(formatter.string(from: first))"
    }
}

enum WordTally {
    struct Day: Sendable {
        let name: String
        let text: String
    }

    static let levels = [("Letter", 1_000), ("Essay", 5_000), ("Novella", 25_000),
                         ("Novel", 100_000), ("Epic", 250_000), ("Saga", 500_000), ("Million", 1_000_000)]

    static func words(_ text: String) -> Int {
        text.unicodeScalars.split(whereSeparator: { $0.properties.isWhitespace }).filter { token in
            token.contains { scalar in
                if scalar.properties.isAlphabetic { return true }
                switch scalar.properties.generalCategory {
                case .decimalNumber, .letterNumber, .otherNumber: return true
                default: return false
                }
            }
        }.count
    }

    static func dayName(_ name: String) -> String? {
        guard name.range(of: #"^[0-9]{4}-[0-9]{2}-[0-9]{2}( [0-9]+)?\.md$"#, options: .regularExpression) != nil else { return nil }
        if name.count > 13 {
            let suffix = name.dropFirst(11).dropLast(3)
            guard let copy = UInt32(suffix), copy >= 2 else { return nil }
        }
        let date = String(name.prefix(10))
        let formatter = dayFormatter()
        guard let parsed = formatter.date(from: date), formatter.string(from: parsed) == date else { return nil }
        return date
    }

    fileprivate static func dayFormatter() -> DateFormatter {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.dateFormat = "yyyy-MM-dd"
        formatter.isLenient = false
        return formatter
    }

    static func compute(days: [Day], speedLines: String, today: Date) -> WordStats {
        let formatter = dayFormatter()
        let todayKey = formatter.string(from: today)
        var total = 0
        var todayWords = 0
        var firstDictationDay: String?
        let headings = try! NSRegularExpression(pattern: #"(?m)^## ([0-9]{2}):([0-9]{2}):([0-9]{2})\n\n"#)
        for day in days {
            guard let key = dayName(day.name) else { continue }
            let text = day.text as NSString
            let matches = headings.matches(in: day.text, range: NSRange(location: 0, length: text.length)).filter {
                Int(text.substring(with: $0.range(at: 1)))! < 24 &&
                Int(text.substring(with: $0.range(at: 2)))! < 60 && Int(text.substring(with: $0.range(at: 3)))! < 60
            }
            if !matches.isEmpty {
                firstDictationDay = min(firstDictationDay ?? key, key)
            }
            for (index, heading) in matches.enumerated() {
                let start = NSMaxRange(heading.range)
                let end = index + 1 < matches.count ? matches[index + 1].range.location : text.length
                let count = words(text.substring(with: NSRange(location: start, length: end - start)))
                total += count
                if key == todayKey { todayWords += count }
            }
        }
        var timedWords = 0.0, spoken = 0.0, todayTimed = 0.0, todaySpoken = 0.0
        for line in speedLines.split(separator: "\n") {
            let fields = line.split(separator: "\t", omittingEmptySubsequences: false)
            guard fields.count == 3, let ms = Int64(fields[0]), let duration = UInt64(fields[1]),
                  let count = UInt64(fields[2]) else { continue }
            timedWords += Double(count)
            spoken += Double(duration)
            if formatter.string(from: Date(timeIntervalSince1970: Double(ms) / 1000)) == todayKey {
                todayTimed += Double(count)
                todaySpoken += Double(duration)
            }
        }
        func speed(_ words: Double, _ ms: Double) -> Int? {
            guard ms >= 60_000 else { return nil }
            let value = (words * 60_000 / ms).rounded()
            guard value.isFinite, value < Double(Int.max) else { return nil }
            return Int(value)
        }
        func round1(_ value: Double) -> Double { (value * 10).rounded() / 10 }
        let wpm = speed(timedWords, spoken)
        let index = levels.prefix { total >= $0.1 }.count
        let at = index == 0 ? 0 : levels[index - 1].1
        let next = index < levels.count ? levels[index] : nil
        return WordStats(total: total, today: todayWords, firstDictationDay: firstDictationDay,
                         wpm: wpm, todayWpm: speed(todayTimed, todaySpoken),
                         timesTyping: wpm.map { round1(Double($0) / 40) },
                         hoursSaved: wpm.flatMap { $0 > 0 ? round1(Double(total) / 40 / 60 - Double(total) / Double($0) / 60) : nil },
                         levelIndex: index, levelName: index == 0 ? nil : levels[index - 1].0,
                         nextName: next?.0, toGo: next.map { $0.1 - total } ?? 0,
                         progress: next.map { Double(total - at) / Double($0.1 - at) } ?? 1)
    }
}

enum StatsSnapshot {
    static var url: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: "group.computer.see.ios")?
            .appendingPathComponent("Library/stats.json")
    }

    static func read() -> WordStats? {
        guard let url, let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(WordStats.self, from: data)
    }
}
