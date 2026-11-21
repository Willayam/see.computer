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
    var releasePoint: CGPoint?

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        releasePoint = touches.first?.location(in: self)
        super.touchesEnded(touches, with: event)
        releasePoint = nil
    }

    let output: String
    private var titleLabel: UILabel?
    private var symbolView: UIImageView?
    private var face: UIView!

    init(title: String? = nil, symbol: String? = nil, label: String? = nil, output: String? = nil) {
        self.output = output ?? title ?? ""
        super.init(frame: .zero)
        // The host hit-tests a remote keyboard surface. Give the touch cell a
        // continuous surface even where the inset keycap leaves a gutter.
        backgroundColor = UIColor(white: 0.5, alpha: 0.02)
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
