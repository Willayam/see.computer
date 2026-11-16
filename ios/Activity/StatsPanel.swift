import SwiftUI

private let statsAccent = Color(red: 0.353, green: 0.784, blue: 0.980)

struct StatsPanel: View, Animatable {
    let value: WordStats
    var fraction: Double = 1
    var compact = false

    var animatableData: Double {
        get { fraction }
        set { fraction = newValue }
    }

    private var amount: Int { Int((Double(value.total) * fraction).rounded()) }
    private var speed: Int? { value.wpm.map { Int((Double($0) * fraction).rounded()) } }

    var body: some View {
        VStack(spacing: 0) {
            ZStack(alignment: .topTrailing) {
                HStack(spacing: compact ? 12 : 14) {
                    ring
                    VStack(alignment: .leading, spacing: 1) {
                        if !compact { level }
                        (Text(amount.formatted()).font(.system(size: compact ? 28 : 40, weight: .bold)).tracking(compact ? -0.8 : -1.2)
                         + Text(" words").font(.system(size: compact ? 14 : 17)).foregroundColor(.secondary))
                            .lineLimit(1)
                            .minimumScaleFactor(0.6)
                            .padding(.trailing, compact ? 52 : 0)
                        if compact {
                            level
                        } else if let next = value.nextName {
                            Text("\(value.toGo.formatted()) to \(next)")
                                .font(.system(size: 13)).foregroundStyle(.secondary)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                if compact {
                    VStack(alignment: .trailing, spacing: 0) {
                        Text(value.hoursSaved.map { String(format: "%.1f h", $0 * fraction) } ?? "–")
                            .font(.system(size: 17, weight: .semibold))
                        Text("saved").font(.system(size: 11)).foregroundStyle(.secondary)
                    }
                }
            }
            .padding(.top, compact ? 0 : 14)
            .padding(.bottom, compact ? 11 : 14)
            Divider()
            StatsBars(wpm: speed, fraction: fraction, compact: compact)
                .padding(.top, compact ? 8 : 12)
                .padding(.bottom, compact ? 0 : 8)
            if !compact {
                Divider()
                footer
            }
        }
        .monospacedDigit()
        .foregroundStyle(.primary)
    }

    private var ring: some View {
        let size: CGFloat = compact ? 54 : 76
        let symbols = ["doc.text", "doc.text", "doc.on.doc", "book.closed", "book", "book.closed.fill", "books.vertical", "books.vertical.fill"]
        return ZStack {
            Circle().stroke(Color(uiColor: .tertiarySystemFill), lineWidth: size * 5 / 72)
            Circle().trim(from: 0, to: value.progress * fraction)
                .stroke(statsAccent, style: StrokeStyle(lineWidth: size * 5 / 72, lineCap: .round))
                .rotationEffect(.degrees(-90))
            Image(systemName: symbols[min(max(value.levelIndex, 0), 7)])
                .font(.system(size: size * 26 / 72, weight: .regular))
        }
        .padding(size * 4 / 72)
        .frame(width: size, height: size)
        .accessibilityLabel("Level \(value.levelIndex), \(Int(value.progress * 100)) percent to next level")
    }

    private var level: some View {
        HStack(spacing: 6) {
            if let name = value.levelName { Text(name).fontWeight(.semibold) }
            Text("Level \(value.levelIndex)").foregroundStyle(.secondary)
            HStack(spacing: 3) {
                ForEach(1...7, id: \.self) { index in
                    Circle().fill(index <= value.levelIndex ? Color.primary : Color(uiColor: .systemFill))
                        .frame(width: 5, height: 5)
                }
            }
            .padding(.leading, 2)
            .accessibilityHidden(true)
        }
        .font(.system(size: 13))
        .lineLimit(1)
        .minimumScaleFactor(0.7)
    }

    private var footer: some View {
        HStack(alignment: .top, spacing: 0) {
            if let times = value.timesTyping {
                figure(String(format: "%.1fx", times * fraction), unit: "", caption: "typing speed")
            } else {
                Text("Speed after a minute of dictation")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.top, 10).padding(.trailing, 6)
            }
            Divider()
            figure(value.hoursSaved.map { String(format: "%.1f", $0 * fraction) } ?? "–", unit: "hours", caption: "saved so far")
                .padding(.leading, 12)
            Divider()
            figure(Int((Double(value.today) * fraction).rounded()).formatted(), unit: "today",
                   caption: value.todayWpm.map { "at \(Int((Double($0) * fraction).rounded())) wpm" } ?? "")
                .padding(.leading, 12)
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding(.bottom, 12)
    }

    private func figure(_ number: String, unit: String, caption: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            (Text(number).font(.system(size: 20, weight: .semibold)).tracking(-0.4)
             + Text(unit.isEmpty ? "" : " \(unit)").font(.system(size: 13)).foregroundColor(.secondary))
                .lineLimit(1).minimumScaleFactor(0.6)
            Text(caption).font(.system(size: 12)).foregroundStyle(.secondary)
                .padding(.top, 1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.top, 10)
        .padding(.trailing, 6)
    }
}

private struct StatsBars: View {
    let wpm: Int?
    let fraction: Double
    let compact: Bool
    private var labelWidth: CGFloat { compact ? 50 : 58 }
    private var valueWidth: CGFloat { compact ? 72 : 82 }

    var body: some View {
        VStack(spacing: 0) {
            row("You", speed: wpm, you: true)
            row("Typing", speed: 40, you: false)
            HStack(spacing: 0) {
                Color.clear.frame(width: labelWidth)
                GeometryReader { geometry in
                    ForEach(2...4, id: \.self) { mark in
                        Text("\(mark)x").font(.system(size: 11)).foregroundStyle(.secondary)
                            .position(x: geometry.size.width * CGFloat(mark) / 4 - (mark == 4 ? 6 : 0), y: 7)
                    }
                }
                Color.clear.frame(width: valueWidth)
            }
            .frame(height: 14)
            .padding(.top, compact ? 0 : 1)
        }
    }

    private func row(_ label: String, speed: Int?, you: Bool) -> some View {
        HStack(spacing: 0) {
            Text(label).foregroundStyle(.secondary).frame(width: labelWidth, alignment: .leading)
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color(uiColor: .tertiarySystemFill))
                    Capsule().fill(you ? statsAccent : Color(uiColor: .tertiaryLabel))
                        .frame(width: geometry.size.width * min(Double(speed ?? 0) / 160, 1) * (you ? 1 : fraction))
                    ForEach(2...3, id: \.self) { mark in
                        Rectangle().fill(Color(uiColor: .secondarySystemGroupedBackground))
                            .frame(width: 1.5)
                            .offset(x: geometry.size.width * CGFloat(mark) / 4)
                    }
                }
            }
            .frame(height: compact ? 7 : 8)
            Group {
                if let speed {
                    Text(speed.formatted()).fontWeight(.semibold) + Text(" wpm").foregroundColor(.secondary)
                } else {
                    Text("–").foregroundColor(.secondary)
                }
            }
            .frame(width: valueWidth, alignment: .trailing)
        }
        .font(.system(size: compact ? 13 : 15))
        .frame(height: compact ? 21 : 24)
        .accessibilityElement(children: .combine)
    }
}
