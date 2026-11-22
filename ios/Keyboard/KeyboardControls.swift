import UIKit

private enum Ink {
    static let key = UIColor {
        $0.userInterfaceStyle == .dark ? UIColor(white: 1, alpha: 0.165) : .white
    }
    static let keyDown = UIColor {
        $0.userInterfaceStyle == .dark ? UIColor(white: 0.24, alpha: 1) : UIColor(red: 0.7, green: 0.72, blue: 0.76, alpha: 1)
    }
    static let mark = UIColor { $0.userInterfaceStyle == .dark ? .white : UIColor(white: 0.11, alpha: 1) }
    static let markDim = mark.withAlphaComponent(0.6)
}

private func keyFace(radius: CGFloat, in control: UIControl, usesGlass: Bool = false, inset: CGSize = .zero) -> UIView {
    let face: UIView
    if #available(iOS 26, *), usesGlass {
        let glass = UIGlassEffect(style: .regular)
        glass.isInteractive = true
        let effectView = UIVisualEffectView(effect: glass)
        effectView.cornerConfiguration = .corners(radius: .fixed(radius))
        face = effectView
    } else {
        face = UIView()
        face.backgroundColor = Ink.key
    }
    face.layer.cornerRadius = radius
    face.layer.cornerCurve = .continuous
    face.isUserInteractionEnabled = false
    face.translatesAutoresizingMaskIntoConstraints = false
    control.addSubview(face)
    NSLayoutConstraint.activate([
        face.leadingAnchor.constraint(equalTo: control.leadingAnchor, constant: inset.width),
        face.trailingAnchor.constraint(equalTo: control.trailingAnchor, constant: -inset.width),
        face.topAnchor.constraint(equalTo: control.topAnchor, constant: inset.height),
        face.bottomAnchor.constraint(equalTo: control.bottomAnchor, constant: -inset.height)
    ])
    return face
}

/// The same target starts and finishes a take. Its glass is inset from both
/// the keyboard edge and the control's full touch area.
final class StartBar: UIControl {
    private let glyph = UIImageView()
    private let label = UILabel()

    override init(frame: CGRect) {
        super.init(frame: frame)
        let cap = UIControl()
        cap.isUserInteractionEnabled = false
        cap.translatesAutoresizingMaskIntoConstraints = false
        addSubview(cap)
        _ = keyFace(radius: 18, in: cap, usesGlass: true)
        let content = UIStackView(arrangedSubviews: [glyph, label])
        content.alignment = .center
        content.spacing = 6
        content.isUserInteractionEnabled = false
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        glyph.tintColor = Ink.mark
        glyph.preferredSymbolConfiguration = .init(pointSize: 16, weight: .semibold)
        label.textColor = Ink.mark
        label.font = .systemFont(ofSize: 16, weight: .semibold)
        NSLayoutConstraint.activate([
            cap.leadingAnchor.constraint(equalTo: leadingAnchor),
            cap.trailingAnchor.constraint(equalTo: trailingAnchor),
            cap.topAnchor.constraint(equalTo: topAnchor, constant: 3),
            cap.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -3),
            content.centerXAnchor.constraint(equalTo: centerXAnchor),
            content.centerYAnchor.constraint(equalTo: centerYAnchor)
        ])
        isAccessibilityElement = true
        accessibilityTraits = .button
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    func show(_ phase: SessionState.Phase) {
        switch phase {
        case .off, .armed:
            glyph.image = UIImage(systemName: "mic.fill")
            label.text = "Dictate"
            accessibilityLabel = "Start dictation"
        case .recording:
            glyph.image = UIImage(systemName: "checkmark")
            label.text = "Done"
            accessibilityLabel = "Finish dictation"
        case .transcribing:
            glyph.image = UIImage(systemName: "ellipsis")
            label.text = "Finishing"
            accessibilityLabel = "Finishing dictation"
        }
        isEnabled = phase != .transcribing
    }
}

final class Key: UIControl {
    static let touchSurfaceColor = UIColor(white: 0.5, alpha: 0.02)
    var releasePoint: CGPoint?

    private var stroke = [SwipeSample]()
    private var swiping = false
    private var travel = TypingKeys.StrokeTravel()
    private var trail: SwipeTrail?

    private func record(_ touch: UITouch, on keyboard: TypingKeys) {
        guard !stroke.isEmpty else { return }
        let sample = SwipeSample(point: touch.location(in: keyboard), time: touch.timestamp)
        if stroke.count >= 512 {
            stroke = stride(from: 0, to: stroke.count, by: 2).map { stroke[$0] }
        }
        stroke.append(sample)
        if swiping {
            trail?.add(sample)
            return
        }
        guard travel.isWordGesture(at: sample.point, start: stroke[0].point, from: self, on: keyboard) else { return }
        guard keyboard.beginSwipe(from: self) else { return }
        swiping = true
        trackedKey?.isHighlighted = false
        trackedKey = nil
        trail = SwipeTrail(on: keyboard, from: stroke)
    }

    private func endStroke() {
        stroke.removeAll(keepingCapacity: true)
        if swiping { typingKeys?.endSwipe(from: self) }
        swiping = false
        travel = TypingKeys.StrokeTravel()
        trail?.lift()
        trail = nil
    }

    private weak var trackedKey: Key?
    private var resolvesAtRelease = false
    var hasActiveTouch: Bool { resolvesAtRelease || isTracking }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        resolvesAtRelease = typingKeys?.resolvesAtRelease(self) ?? false
        if resolvesAtRelease {
            if let keyboard = typingKeys, keyboard.canSwipe(from: self), let touch = touches.first {
                stroke = [SwipeSample(point: touch.location(in: keyboard), time: touch.timestamp)]
            }
            trackedKey = self
            isHighlighted = true
        } else {
            super.touchesBegan(touches, with: event)
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard resolvesAtRelease else {
            super.touchesMoved(touches, with: event)
            return
        }
        guard let keyboard = typingKeys, let touch = touches.first else { return }
        record(touch, on: keyboard)
        guard !swiping, let next = keyboard.releaseKey(at: touch.location(in: keyboard)), next !== trackedKey else { return }
        trackedKey?.isHighlighted = false
        trackedKey = next
        next.isHighlighted = true
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        endStroke()
        if resolvesAtRelease { finishHighlight() } else { super.touchesCancelled(touches, with: event) }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard resolvesAtRelease else {
            releasePoint = touches.first?.location(in: self)
            super.touchesEnded(touches, with: event)
            releasePoint = nil
            return
        }
        defer { endStroke() }
        if let keyboard = typingKeys, let touch = touches.first {
            record(touch, on: keyboard)
            if swiping {
                finishHighlight()
                keyboard.finishSwipe(stroke)
                return
            }
            if keyboard.finishShortSwipe(stroke, from: self) {
                finishHighlight()
                return
            }
        }
        let last = trackedKey
        finishHighlight()
        guard let keyboard = typingKeys, let touch = touches.first else { return }
        keyboard.release(from: self, last: last, at: touch.location(in: keyboard))
    }

    private func finishHighlight() {
        trackedKey?.isHighlighted = false
        trackedKey = nil
        resolvesAtRelease = false
    }

    let output: String
    private var titleLabel: UILabel?
    private var symbolView: UIImageView?
    private var face: UIView!

    func faceFrame(in view: UIView) -> CGRect { face.convert(face.bounds, to: view) }

    init(title: String? = nil, symbol: String? = nil, label: String? = nil, output: String? = nil) {
        self.output = output ?? title ?? ""
        super.init(frame: .zero)
        // The host hit-tests a remote keyboard surface. Give the touch cell a
        // continuous surface even where the inset keycap leaves a gutter.
        backgroundColor = Self.touchSurfaceColor
        face = keyFace(radius: 8, in: self, inset: CGSize(width: 3, height: 5.5))
        let content: UIView?
        if let symbol {
            let image = UIImageView(image: UIImage(systemName: symbol))
            image.tintColor = Ink.mark
            image.preferredSymbolConfiguration = .init(pointSize: 19, weight: .regular)
            symbolView = image
            content = image
        } else if let title, !title.isEmpty {
            let text = UILabel()
            text.text = title
            text.textColor = Ink.mark
            text.font = .systemFont(ofSize: title.count > 1 ? 18 : 22)
            titleLabel = text
            content = text
        } else {
            content = nil
        }
        if let content {
            content.translatesAutoresizingMaskIntoConstraints = false
            content.isUserInteractionEnabled = false
            addSubview(content)
            content.centerXAnchor.constraint(equalTo: centerXAnchor).isActive = true
            content.centerYAnchor.constraint(equalTo: centerYAnchor).isActive = true
        }
        isAccessibilityElement = true
        accessibilityTraits = .keyboardKey
        accessibilityLabel = label ?? title
    }

    func setTitle(_ title: String) {
        titleLabel?.text = title
        accessibilityLabel = title
    }

    private var restingColor = Ink.key

    func setActionColor(_ accent: Bool) {
        restingColor = accent ? .systemBlue : Ink.key
        face.backgroundColor = restingColor
        symbolView?.tintColor = accent ? .white : Ink.mark
    }

    func setSymbol(_ symbol: String) {
        symbolView?.image = UIImage(systemName: symbol)
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    private var typingKeys: TypingKeys? {
        var ancestor = superview
        while let view = ancestor {
            if let keys = view as? TypingKeys { return keys }
            ancestor = view.superview
        }
        return nil
    }

    override var isHighlighted: Bool {
        didSet {
            guard oldValue != isHighlighted else { return }
            face.backgroundColor = isHighlighted ? Ink.keyDown : restingColor
            guard output.count == 1, let character = titleLabel?.text, character.count == 1,
                let keyboard = typingKeys else { return }
            if isHighlighted {
                keyboard.preview.show(character, for: self, in: keyboard)
            } else {
                keyboard.preview.hide(for: self)
            }
        }
    }
}

/// The slide-to-type trail, matched to Apple's iOS 27 keyboard in the simulator:
/// a translucent grey comet with a round head under the finger. Width falls off
/// with both path distance and age, so a fast stroke leaves a longer, slimmer
/// tail. Once the finger stops or lifts, the comet shrinks to a short stub and
/// thins out. The trail keeps itself alive through its display link until then.
final class SwipeTrail: NSObject {
    private struct Mark {
        let point: CGPoint
        let time: TimeInterval
        let distance: CGFloat
    }

    static let headWidth: CGFloat = 20
    static let taperLength: CGFloat = 313
    static let taperAge: TimeInterval = 0.43
    static let fadeDuration: TimeInterval = 0.62
    static let needleWidth: CGFloat = 2
    static let stubLength: CGFloat = 32
    static let stubTime: TimeInterval = 0.22
    // Touches arrive every 8 ms on device but every 33 ms in the simulator;
    // a shorter gap is not a stop.
    static let stillAfter: TimeInterval = 0.034

    private let shape = CAShapeLayer()
    private var marks: [Mark] = []
    private var link: CADisplayLink?
    private var lifted = false

    init(on view: UIView, from samples: [SwipeSample]) {
        super.init()
        let dark = view.traitCollection.userInterfaceStyle == .dark
        shape.fillColor = (dark
            ? UIColor(red: 157 / 255, green: 161 / 255, blue: 173 / 255, alpha: 0.52)
            : UIColor(red: 154 / 255, green: 160 / 255, blue: 168 / 255, alpha: 0.40)).cgColor
        shape.zPosition = 1
        view.layer.addSublayer(shape)
        samples.forEach(add)
        let link = CADisplayLink(target: self, selector: #selector(step))
        link.preferredFrameRateRange = CAFrameRateRange(minimum: 60, maximum: 120, preferred: 120)
        link.add(to: .main, forMode: .common)
        self.link = link
        draw(at: CACurrentMediaTime())
    }

    func add(_ sample: SwipeSample) {
        guard let last = marks.last else {
            marks.append(Mark(point: sample.point, time: sample.time, distance: 0))
            return
        }
        let step = hypot(sample.point.x - last.point.x, sample.point.y - last.point.y)
        guard step > 0.25 else { return }
        marks.append(Mark(point: sample.point, time: sample.time, distance: last.distance + step))
        link?.isPaused = false
    }

    func lift() {
        lifted = true
        link?.isPaused = false
    }

    @objc private func step(_ link: CADisplayLink) {
        draw(at: link.targetTimestamp)
    }

    private func draw(at now: TimeInterval) {
        guard let head = marks.last else { return finish() }
        let idle = max(0, now - head.time - (lifted ? 0 : Self.stillAfter))
        let fade = max(0, 1 - idle / Self.fadeDuration)
        if fade == 0 {
            if lifted { return finish() }
            // A finger resting long enough leaves nothing to draw until it moves.
            link?.isPaused = true
        }

        var body: [(point: CGPoint, distance: CGFloat, taper: CGFloat)] = []
        var oldest = 0
        for index in marks.indices.reversed() {
            let mark = marks[index]
            let taper = 1 - (head.distance - mark.distance) / Self.taperLength - CGFloat(head.time - mark.time) / CGFloat(Self.taperAge)
            if taper <= 0 {
                if let previous = body.last {
                    let t = previous.taper / (previous.taper - taper)
                    let point = CGPoint(x: previous.point.x + (mark.point.x - previous.point.x) * t,
                                        y: previous.point.y + (mark.point.y - previous.point.y) * t)
                    body.append((point, previous.distance + (head.distance - mark.distance - previous.distance) * t, 0))
                }
                oldest = index
                break
            }
            body.append((mark.point, head.distance - mark.distance, taper))
        }
        if oldest > 0 { marks.removeFirst(oldest) }

        // A resting comet settles to a short stub instead of keeping its tail.
        var stub = CGFloat.infinity
        if idle > 0, let length = body.last?.distance, length > Self.stubLength {
            stub = Self.stubLength + (length - Self.stubLength) * CGFloat(exp(-idle / Self.stubTime))
        }
        let scale = CGFloat(fade) / 2
        var outline: [(point: CGPoint, radius: CGFloat)] = []
        for (index, part) in body.enumerated() {
            var taper = part.taper
            if stub.isFinite {
                taper = min(taper, 1 - part.distance / stub)
                if taper <= 0, index > 0 {
                    let previous = body[index - 1]
                    let before = min(previous.taper, 1 - previous.distance / stub)
                    let t = before / (before - taper)
                    outline.append((CGPoint(x: previous.point.x + (part.point.x - previous.point.x) * t,
                                            y: previous.point.y + (part.point.y - previous.point.y) * t), Self.needleWidth * scale))
                    break
                }
            }
            outline.append((part.point, max(Self.needleWidth, Self.headWidth * taper) * scale))
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        shape.path = Self.path(through: Self.smoothed(outline))
        CATransaction.commit()
    }

    private func finish() {
        link?.invalidate()
        link = nil
        shape.removeFromSuperlayer()
    }

    /// Catmull-Rom points between sparse touches, so a 30 Hz stroke still
    /// rounds its corners the way Apple's does.
    static func smoothed(_ points: [(point: CGPoint, radius: CGFloat)]) -> [(point: CGPoint, radius: CGFloat)] {
        guard points.count > 2 else { return points }
        var result = [points[0]]
        for index in 0 ..< points.count - 1 {
            let p0 = points[max(0, index - 1)].point, p1 = points[index].point
            let p2 = points[index + 1].point, p3 = points[min(points.count - 1, index + 2)].point
            let pieces = min(8, Int(hypot(p2.x - p1.x, p2.y - p1.y) / 3))
            if pieces > 1 {
                for piece in 1 ..< pieces {
                    let t = CGFloat(piece) / CGFloat(pieces), t2 = t * t, t3 = t2 * t
                    func blend(_ a: CGFloat, _ b: CGFloat, _ c: CGFloat, _ d: CGFloat) -> CGFloat {
                        0.5 * (2 * b + (c - a) * t + (2 * a - 5 * b + 4 * c - d) * t2 + (3 * b - a - 3 * c + d) * t3)
                    }
                    let radius = points[index].radius + (points[index + 1].radius - points[index].radius) * t
                    result.append((CGPoint(x: blend(p0.x, p1.x, p2.x, p3.x), y: blend(p0.y, p1.y, p2.y, p3.y)), radius))
                }
            }
            result.append(points[index + 1])
        }
        return result
    }

    /// The union of a disc at every point and the tangent hull between each
    /// pair. Every piece winds the same way, so nonzero filling never punches
    /// a hole where the trail crosses itself.
    static func path(through points: [(point: CGPoint, radius: CGFloat)]) -> CGPath {
        let path = CGMutablePath()
        for (index, current) in points.enumerated() where current.radius > 0.05 {
            path.move(to: CGPoint(x: current.point.x + current.radius, y: current.point.y))
            path.addArc(center: current.point, radius: current.radius, startAngle: 0, endAngle: -2 * .pi, clockwise: true)
            path.closeSubpath()
            guard index + 1 < points.count else { continue }
            let next = points[index + 1]
            let dx = next.point.x - current.point.x, dy = next.point.y - current.point.y
            let length = hypot(dx, dy)
            guard length > abs(current.radius - next.radius) else { continue }
            let ux = dx / length, uy = dy / length
            let e = (current.radius - next.radius) / length, s = (1 - e * e).squareRoot()
            let left = CGPoint(x: e * ux - s * uy, y: e * uy + s * ux)
            let right = CGPoint(x: e * ux + s * uy, y: e * uy - s * ux)
            path.move(to: CGPoint(x: current.point.x + current.radius * left.x, y: current.point.y + current.radius * left.y))
            path.addLine(to: CGPoint(x: next.point.x + next.radius * left.x, y: next.point.y + next.radius * left.y))
            path.addLine(to: CGPoint(x: next.point.x + next.radius * right.x, y: next.point.y + next.radius * right.y))
            path.addLine(to: CGPoint(x: current.point.x + current.radius * right.x, y: current.point.y + current.radius * right.y))
            path.closeSubpath()
        }
        return path
    }
}

/// Reuse one material and label for all letters. Overlapping thumb presses
/// release independently, so only the key that owns the preview can hide it.
final class KeyPreview: UIControl {
    private let label = UILabel()
    private weak var owner: Key?

    override init(frame: CGRect) {
        super.init(frame: frame)
        isUserInteractionEnabled = false
        accessibilityElementsHidden = true
        _ = keyFace(radius: 12, in: self, usesGlass: true)
        label.textAlignment = .center
        label.textColor = Ink.mark
        label.font = .systemFont(ofSize: 29)
        addSubview(label)
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    func show(_ character: String, for key: Key, in keyboard: TypingKeys) {
        owner = key
        let rect = key.convert(key.bounds, to: keyboard)
        let width = max(46, rect.width + 4)
        let x = min(max(0, rect.midX - width / 2), keyboard.bounds.width - width)
        frame = CGRect(x: x, y: rect.minY - 40, width: width, height: 46)
        label.frame = bounds
        label.text = character
        keyboard.addSubview(self)
    }

    func hide(for key: Key) {
        if key === owner { removeFromSuperview() }
    }
}

/// How loud the microphone hears you, from the level the app writes.
final class Meter: UIStackView {
    private var heights: [NSLayoutConstraint] = []
    private var levels = [Float](repeating: 0, count: 7)
    private var link: CADisplayLink?
    private var frames = 0

    override init(frame: CGRect) {
        super.init(frame: frame)
        axis = .horizontal
        alignment = .center
        spacing = 3
        for _ in levels {
            let bar = UIView()
            bar.layer.cornerRadius = 1.5
            bar.backgroundColor = Ink.mark.withAlphaComponent(0.75)
            bar.translatesAutoresizingMaskIntoConstraints = false
            bar.widthAnchor.constraint(equalToConstant: 3).isActive = true
            let height = bar.heightAnchor.constraint(equalToConstant: 3)
            height.isActive = true
            heights.append(height)
            addArrangedSubview(bar)
        }
        heightAnchor.constraint(equalToConstant: 24).isActive = true
    }

    required init(coder: NSCoder) { fatalError("not used") }

    func start() {
        guard link == nil else { return }
        let link = CADisplayLink(target: self, selector: #selector(tick))
        link.preferredFrameRateRange = CAFrameRateRange(minimum: 15, maximum: 30, preferred: 30)
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    func stop() {
        link?.invalidate()
        link = nil
        levels = [Float](repeating: 0, count: levels.count)
        heights.forEach { $0.constant = 3 }
    }

    @objc private func tick() {
        frames += 1
        guard frames % 2 == 0 else { return }
        levels.removeFirst()
        levels.append(Bridge.readLevel())
        for (index, height) in heights.enumerated() {
            height.constant = 3 + CGFloat(levels[index]) * 19
        }
    }
}

final class CloseButton: UIControl {
    private let glyph = UIImageView(image: UIImage(systemName: "xmark"))

    override init(frame: CGRect) {
        super.init(frame: frame)
        glyph.tintColor = Ink.markDim
        glyph.preferredSymbolConfiguration = .init(pointSize: 15, weight: .bold)
        glyph.translatesAutoresizingMaskIntoConstraints = false
        glyph.isUserInteractionEnabled = false
        addSubview(glyph)
        glyph.centerXAnchor.constraint(equalTo: centerXAnchor).isActive = true
        glyph.centerYAnchor.constraint(equalTo: centerYAnchor).isActive = true
        accessibilityLabel = "Cancel"
        isAccessibilityElement = true
        accessibilityTraits = .button
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override var isHighlighted: Bool {
        didSet { glyph.alpha = isHighlighted ? 0.5 : 1 }
    }
}
