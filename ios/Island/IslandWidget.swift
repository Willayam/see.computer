import ActivityKit
import AppIntents
import SwiftUI
import WidgetKit

@main
struct IslandWidgets: WidgetBundle {
    var body: some Widget {
        TakeLiveActivity()
    }
}

private let accent = Color(red: 0.353, green: 0.784, blue: 0.980)

struct TakeLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: TakeActivity.self) { context in
            LockScreen(state: context.state)
                .padding(16)
        } dynamicIsland: { context in
            let state = context.state
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Mark(state: state, height: 26)
                        .font(.title2)
                        .padding(.leading, 6)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Clock(state: state)
                        .font(.title2.weight(.semibold))
                        .padding(.trailing, 6)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    Action(phase: state.phase)
                }
            } compactLeading: {
                Mark(state: state, height: 18)
            } compactTrailing: {
                Clock(state: state)
            } minimal: {
                Mark(state: state, height: 16)
            }
            .keylineTint(accent)
        }
    }
}

private struct Mark: View {
    let state: TakeActivity.ContentState
    let height: CGFloat

    var body: some View {
        switch state.phase {
        case .ready:
            Image(systemName: "mic").foregroundStyle(.secondary)
        case .recording:
            Bars(levels: state.levels ?? [3, 6, 9, 6, 3], height: height)
        case .writing:
            Image(systemName: "text.cursor").foregroundStyle(accent)
        }
    }
}

/// The last second of the mic, like the keyboard's meter. iOS draws a Live
/// Activity once per update, so these move once a second and it animates between.
private struct Bars: View {
    let levels: [Int]
    let height: CGFloat

    var body: some View {
        HStack(alignment: .center, spacing: height / 7) {
            ForEach(Array(levels.enumerated()), id: \.offset) { _, level in
                Capsule()
                    .fill(accent)
                    .frame(width: height / 6, height: height * (0.2 + 0.8 * CGFloat(min(max(level, 0), 10)) / 10))
            }
        }
        .frame(height: height)
        .animation(.spring(duration: 0.6), value: levels)
    }
}

/// Counts up on its own, so the app does not have to update it every second.
private struct Clock: View {
    let state: TakeActivity.ContentState

    var body: some View {
        if state.phase == .recording, let since = state.since {
            Text(since, style: .timer)
                .monospacedDigit()
                .multilineTextAlignment(.trailing)
                .frame(maxWidth: 52, alignment: .trailing)
        } else {
            Text(state.phase == .writing ? "Writing" : "Ready")
                .foregroundStyle(.secondary)
        }
    }
}

private struct Action: View {
    let phase: TakeActivity.ContentState.Phase

    var body: some View {
        switch phase {
        case .recording:
            Button(intent: FinishTakeIntent()) {
                Label("Done", systemImage: "checkmark")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
            }
            .tint(accent)
        case .ready:
            Button(intent: TurnOffMicIntent()) {
                Label("Turn off mic", systemImage: "mic.slash")
                    .frame(maxWidth: .infinity)
            }
            .tint(.gray)
        case .writing:
            EmptyView()
        }
    }
}

private struct LockScreen: View {
    let state: TakeActivity.ContentState

    var body: some View {
        HStack(spacing: 12) {
            Mark(state: state, height: 26).font(.title2)
            Clock(state: state).font(.title3.weight(.semibold))
            Spacer()
            Action(phase: state.phase).fixedSize()
        }
    }
}
