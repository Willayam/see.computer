import SwiftUI
import WidgetKit

private struct StatsEntry: TimelineEntry {
    let date: Date
    let value: WordStats
    let isPlaceholder: Bool
}

private struct StatsProvider: TimelineProvider {
    func placeholder(in context: Context) -> StatsEntry {
        StatsEntry(date: Date(), value: WordStats(total: 117302, today: 473, wpm: 119, todayWpm: 116,
                   timesTyping: 3, hoursSaved: 32.5, levelIndex: 4, levelName: "Novel", nextName: "Epic",
                   toGo: 132698, progress: 17302.0 / 150000), isPlaceholder: true)
    }

    func getSnapshot(in context: Context, completion: @escaping (StatsEntry) -> Void) {
        completion(StatsSnapshot.read().map { StatsEntry(date: Date(), value: $0, isPlaceholder: false) } ?? placeholder(in: context))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<StatsEntry>) -> Void) {
        getSnapshot(in: context) { entry in
            completion(Timeline(entries: [entry], policy: .after(Date().addingTimeInterval(3600))))
        }
    }
}

struct WordStatsWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "WordStats", provider: StatsProvider()) { entry in
            StatsPanel(value: entry.value, compact: true)
                .redacted(reason: entry.isPlaceholder ? .placeholder : [])
                .containerBackground(.background, for: .widget)
        }
        .configurationDisplayName("Words")
        .description("Your words, level and dictation speed.")
        .supportedFamilies([.systemMedium])
    }
}
