import ActivityKit
import AppIntents
import Foundation

/// The mic on the Lock Screen and in the Dynamic Island. Compiled into the app and
/// the Island extension.
struct TakeActivity: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        enum Phase: String, Codable {
            case ready, recording, writing
        }

        var phase: Phase
        /// When the take started, for the clock.
        var since: Date?
        /// The last second of mic level, oldest first, each from 0 to 10.
        var levels: [Int]?
    }
}

/// iOS runs these in the app, waking it if it has to.
struct FinishTakeIntent: LiveActivityIntent {
    static var title: LocalizedStringResource = "Finish dictation"

    func perform() async throws -> some IntentResult {
        await MainActor.run { NotificationCenter.default.post(name: .finishFromIsland, object: nil) }
        return .result()
    }
}

struct TurnOffMicIntent: LiveActivityIntent {
    static var title: LocalizedStringResource = "Turn off the mic"

    func perform() async throws -> some IntentResult {
        await MainActor.run { NotificationCenter.default.post(name: .turnOffFromIsland, object: nil) }
        return .result()
    }
}

extension Notification.Name {
    static let finishFromIsland = Notification.Name("computer.see.ios.finishFromIsland")
    static let turnOffFromIsland = Notification.Name("computer.see.ios.turnOffFromIsland")
}
