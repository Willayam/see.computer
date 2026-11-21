import UIKit

/// The letter rows stay in place while dictation runs. Only a page or language
/// change rebuilds them, so changing case never interrupts a touch.
final class TypingKeys: UIStackView {
    let preview = KeyPreview()
    private enum Page { case letters, numbers, symbols }
    private var page = Page.letters
    private var letterKeys: [Key] = []
    private weak var spaceKey: Key?
    private weak var deleteKey: Key?
    private var shiftKey: Key?
    private let enter = Key(symbol: "return.left", label: "Return", output: "\n")
    private var returnType: UIReturnKeyType?
    private(set) var uppercase = false
    private var capsLock = false
    private var lastShift = Date.distantPast
    private var swedish = false
    private let globe = Key(symbol: "globe", label: "Next keyboard")

    var keyPrior: (() -> (probabilities: [Double], spread: Double)?)?
    var moveCursor: ((Int) -> Void)?
    private var trackpadX: CGFloat = 0
    var insert: ((String, CGPoint?) -> Void)?
    var startDelete: (() -> Void)?
    var endDelete: (() -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        axis = .vertical
        distribution = .fillEqually
        spacing = 0
        enter.addTarget(self, action: #selector(typed(_:)), for: .touchUpInside)
        rebuild()
    }

    required init(coder: NSCoder) { fatalError("not used") }

    private var lastLetterLayout = KeyLayout()
    var keyLayout: KeyLayout {
        guard let first = letterKeys.first else { return lastLetterLayout }
        var layout = KeyLayout()
        layout.pitch = max(first.bounds.width, 1)
        layout.reach = bounds
        layout.space = frame(of: "Space") ?? .zero
        for key in letterKeys {
            if let ch = key.output.first, let i = Lexicon.codes[ch] {
                let rect = key.convert(key.bounds, to: self)
                layout.centers[i] = CGPoint(x: rect.midX, y: rect.midY)
            }
        }
        return layout
    }

    func connectGlobe(to controller: UIInputViewController) {
        globe.addTarget(controller, action: #selector(UIInputViewController.handleInputModeList(from:with:)), for: .allTouchEvents)
    }

    func configure(swedish: Bool, needsGlobe: Bool) {
        globe.isHidden = !needsGlobe
        guard self.swedish != swedish else { return }
        self.swedish = swedish
        rebuild()
    }

    func reset() {
        capsLock = false
        lastShift = .distantPast
        setUppercase(false)
        if page != .letters {
            page = .letters
            rebuild()
        }
    }

    func updateCase(for proxy: UITextDocumentProxy) {
        if proxy.returnKeyType != returnType {
            returnType = proxy.returnKeyType
            let action: (String, String)
            switch returnType {
            case .search, .google, .yahoo: action = ("magnifyingglass", "Search")
            case .send: action = ("arrow.up", "Send")
            case .go, .next, .continue: action = ("arrow.right", "Continue")
            case .done: action = ("checkmark", "Done")
            case .join: action = ("arrow.right", "Join")
            default: action = ("return.left", "Return")
            }
            enter.setSymbol(action.0)
            enter.accessibilityLabel = action.1
            enter.setActionColor(action.1 != "Return")
        }
        guard !capsLock else { return }
        let before = proxy.documentContextBeforeInput ?? ""
        let capitals: Bool
        switch proxy.autocapitalizationType {
        case .allCharacters: capitals = true
        case .words: capitals = before.last?.isWhitespace ?? true
        case .sentences:
            let trimmed = before.trimmingCharacters(in: .whitespaces)
            capitals = trimmed.isEmpty || trimmed.last?.isNewline == true ||
                (before.last?.isWhitespace == true && trimmed.last.map { ".!?".contains($0) } == true)
        default: capitals = false
        }
        if capitals != uppercase { setUppercase(capitals) }
    }

    private func setUppercase(_ value: Bool) {
        uppercase = value
        for key in letterKeys {
            key.setTitle(value ? key.output.uppercased() : key.output)
        }
        shiftKey?.setSymbol(capsLock ? "capslock.fill" : (value ? "shift.fill" : "shift"))
        shiftKey?.accessibilityValue = capsLock ? "Caps lock" : (value ? "On" : "Off")
    }

    private func rebuild() {
        endDelete?()
        preview.removeFromSuperview()
        arrangedSubviews.forEach { $0.removeFromSuperview() }
        letterKeys = []
        shiftKey = nil
        let rows: [String]
        switch page {
        case .letters: rows = [swedish ? "qwertyuiopå" : "qwertyuiop", swedish ? "asdfghjklöä" : "asdfghjkl", "zxcvbnm"]
        case .numbers: rows = ["1234567890", "-/:;()$&@\"", ".,?!’"]
        case .symbols: rows = ["[]{}#%^*+=", "_\\|~<>€£¥•", ".,?!'`"]
        }
        for (index, characters) in rows.enumerated() {
            let controls = characters.map { typingKey(String($0)) }
            let row = addRow()
            let cluster = UIStackView(arrangedSubviews: controls)
            cluster.distribution = .fillEqually
            let columns = page == .letters && swedish ? 11 : 10
            if page == .letters { letterKeys += controls }
            if index == 2 {
                let modifier: Key
                if page == .letters {
                    modifier = Key(symbol: "shift", label: "Shift")
                    shiftKey = modifier
                    modifier.addTarget(self, action: #selector(shift), for: .touchUpInside)
                } else {
                    modifier = Key(title: page == .numbers ? "#+=" : "123")
                    modifier.addTarget(self, action: #selector(changeSymbols), for: .touchUpInside)
                }
                let delete = Key(symbol: "delete.left", label: "Delete")
                deleteKey = delete
                delete.addTarget(self, action: #selector(deleteDown), for: .touchDown)
                delete.addTarget(self, action: #selector(deleteUp), for: [.touchUpInside, .touchUpOutside, .touchCancel])
                row.addArrangedSubview(modifier)
                row.addArrangedSubview(UIView())
                row.addArrangedSubview(cluster)
                row.addArrangedSubview(UIView())
                row.addArrangedSubview(delete)
                modifier.widthAnchor.constraint(equalTo: row.widthAnchor, multiplier: page == .letters && swedish ? 0.105 : 0.118, constant: 5).isActive = true
                delete.widthAnchor.constraint(equalTo: modifier.widthAnchor).isActive = true
            } else {
                row.addArrangedSubview(cluster)
            }
            cluster.centerXAnchor.constraint(equalTo: row.centerXAnchor).isActive = true
            if index < 2 && controls.count < columns {
                let left = UIView()
                row.insertArrangedSubview(left, at: 0)
                row.addArrangedSubview(UIView())
            }
            let fraction: CGFloat = page != .letters && index == 2 ? 0.70 : CGFloat(controls.count) / CGFloat(columns)
            cluster.widthAnchor.constraint(equalTo: row.widthAnchor, multiplier: fraction).isActive = true
        }
        let mode = Key(title: page == .letters ? "123" : "ABC")
        mode.addTarget(self, action: #selector(changePage), for: .touchUpInside)
        let space = Key(title: "", label: "Space", output: " ")
        spaceKey = space
        space.addTarget(self, action: #selector(typed(_:)), for: .touchUpInside)
        let language = UILabel()
        language.text = swedish ? "SV" : "EN"
        language.font = .systemFont(ofSize: 10)
        language.textColor = .tertiaryLabel
        language.isAccessibilityElement = false
        language.translatesAutoresizingMaskIntoConstraints = false
        space.addSubview(language)
        NSLayoutConstraint.activate([
            language.trailingAnchor.constraint(equalTo: space.trailingAnchor, constant: -8),
            language.bottomAnchor.constraint(equalTo: space.bottomAnchor, constant: -8)
        ])
        let trackpad = UILongPressGestureRecognizer(target: self, action: #selector(trackCursor(_:)))
        trackpad.delaysTouchesEnded = false
        trackpad.minimumPressDuration = 0.35
        space.addGestureRecognizer(trackpad)
        let bottom = addRow()
        let bottomKeys = [mode, globe, space, enter]
        bottomKeys.forEach(bottom.addArrangedSubview)
        for key in [mode, globe] {
            let width = key.widthAnchor.constraint(equalTo: bottom.widthAnchor, multiplier: 0.111, constant: 5)
            width.priority = .defaultHigh
            width.isActive = true
        }
        enter.widthAnchor.constraint(equalTo: bottom.widthAnchor, multiplier: 0.239, constant: 4.5).isActive = true
        setUppercase(uppercase)
    }

    @objc private func trackCursor(_ gesture: UILongPressGestureRecognizer) {
        let x = gesture.location(in: self).x
        switch gesture.state {
        case .began:
            trackpadX = x
            UISelectionFeedbackGenerator().selectionChanged()
        case .changed:
            let steps = Int((x - trackpadX) / 8)
            guard steps != 0 else { return }
            moveCursor?(steps)
            trackpadX += CGFloat(steps) * 8
        default: break
        }
    }

    /// A touch between keys goes to the nearest key, as on the system keyboard.
    override func hitTest(_ point: CGPoint, with event: UIEvent?) -> UIView? {
        guard bounds.contains(point) else { return nil }
        let hit = geometricHit(point, event: event)
        guard let key = hit as? Key else { return hit }
        return resolved(key, at: point)
    }

    private func resolved(_ key: Key, at point: CGPoint) -> Key {
        guard page == .letters else { return key }
        let rect = key.convert(key.bounds, to: self)
        let isLetter = letterKeys.contains(key)
        guard isLetter || key.output == " " || key === enter || key === shiftKey || key.accessibilityLabel == "Delete"
        else { return key }
        if isLetter && rect.insetBy(dx: rect.width * 0.2, dy: rect.height * 0.2).contains(point) { return key }
        let functions = [spaceKey, enter, shiftKey, deleteKey].compactMap { $0 }
        let nearby = functions.filter {
            let f = $0.convert($0.bounds, to: self)
            guard f.insetBy(dx: -8, dy: -8).contains(point) else { return false }
            if point.y <= f.minY + 8 { return true }
            if $0 === shiftKey { return point.x >= f.maxX - 8 }
            if $0 === deleteKey { return point.x <= f.minX + 8 }
            return false
        }
        guard !nearby.isEmpty, let prior = keyPrior?() else { return key }
        let layout = keyLayout
        let sigma = prior.spread * layout.pitch
        let scale = 0.5 / (sigma * sigma)
        func logPrior(_ i: Int) -> Double { log(0.95 * prior.probabilities[i] + 0.05 / 30) }
        var best = -Double.infinity
        var choice = key
        for letter in letterKeys {
            guard let ch = letter.output.first, let i = Lexicon.codes[ch], let c = layout.centers[i] else { continue }
            let dx = point.x - c.x, dy = point.y - c.y
            let cost = (dx * dx + dy * dy) * scale
            let score = logPrior(i) - cost
            if cost <= 10 && score > best { best = score; choice = letter }
        }
        for fn in nearby {
            let score: Double
            if fn === spaceKey {
                score = logPrior(30) - layout.spaceCost(point, sigma: sigma)
            } else {
                let f = fn.convert(fn.bounds, to: self).insetBy(dx: 10, dy: 10)
                let dx = max(0, f.minX - point.x, point.x - f.maxX)
                let dy = max(0, f.minY - point.y, point.y - f.maxY)
                let p = fn === enter ? 0.002 + 0.1 * prior.probabilities[30] : (fn === deleteKey ? 0.05 : 0.002)
                score = log(p) - (dx * dx + dy * dy) * scale
            }
            if score > best { best = score; choice = fn }
        }
        return choice
    }

    private func geometricHit(_ point: CGPoint, event: UIEvent?) -> UIView? {
        let hit = super.hitTest(point, with: event)
        guard let hit, !(hit is Key), hit !== preview, !hit.isDescendant(of: preview) else { return hit }
        var nearest: (key: Key, distance: CGFloat)?
        var pending: [UIView] = [self]
        while let view = pending.popLast() {
            if let key = view as? Key {
                guard !key.isHidden else { continue }
                let frame = key.convert(key.bounds, to: self)
                let dx = max(frame.minX - point.x, 0, point.x - frame.maxX)
                let dy = max(frame.minY - point.y, 0, point.y - frame.maxY)
                let distance = dx * dx + dy * dy
                if distance < nearest?.distance ?? .infinity { nearest = (key, distance) }
            } else {
                pending += view.subviews
            }
        }
        return nearest?.key ?? hit
    }

    /// Where a key sits in this view, found by what it types or by its label.
    func frame(of key: String) -> CGRect? {
        var pending: [UIView] = [self]
        while let view = pending.popLast() {
            if let control = view as? Key, !control.isHidden, control.output == key || control.accessibilityLabel == key {
                return control.convert(control.bounds, to: self)
            }
            pending += view.subviews
        }
        return nil
    }

    func resolvesAtRelease(_ key: Key, at point: CGPoint) -> Bool {
        guard page == .letters else { return false }
        if key === deleteKey {
            let rect = key.convert(key.bounds, to: self)
            return point.y <= rect.minY + 8 || point.x <= rect.minX + 8
        }
        return key === spaceKey || key === enter || key === shiftKey || letterKeys.contains(key)
    }

    func releaseKey(at point: CGPoint) -> Key? {
        guard let key = hitTest(point, with: nil) as? Key,
              key === deleteKey || resolvesAtRelease(key, at: point) else { return nil }
        return key
    }

    /// A finished touch at a point in this view, as the touch system delivers it.
    func tap(at point: CGPoint) {
        var view = hitTest(point, with: nil)
        while let candidate = view, candidate !== self, !(candidate is Key) { view = candidate.superview }
        guard let key = view as? Key else { return }
        activate(key, at: point)
    }

    func activate(_ key: Key, at point: CGPoint) {
        if key === globe { return }
        if key === deleteKey {
            startDelete?()
            endDelete?()
            return
        }
        key.releasePoint = key.convert(point, from: self)
        defer { key.releasePoint = nil }
        // Calls the targets directly. sendActions needs a UIApplication, which the simulator harness lacks.
        for case let target as NSObject in key.allTargets {
            for action in key.actions(forTarget: target, forControlEvent: .touchUpInside) ?? [] {
                target.perform(Selector(action), with: key)
            }
        }
    }

    private func addRow() -> UIStackView {
        let row = UIStackView()
        addArrangedSubview(row)
        return row
    }

    private func typingKey(_ title: String) -> Key {
        let key = Key(title: title)
        key.addTarget(self, action: #selector(typed(_:)), for: .touchUpInside)
        return key
    }

    @objc private func typed(_ key: Key) {
        lastShift = .distantPast
        let value = page == .letters && uppercase ? key.output.uppercased() : key.output
        insert?(value, key.releasePoint.map { key.convert($0, to: self) })
        if page != .letters && (value == " " || value == "’") {
            page = .letters
            rebuild()
        }
    }

    @objc private func shift() {
        let now = Date()
        if capsLock {
            capsLock = false
            setUppercase(false)
        } else if now.timeIntervalSince(lastShift) < 0.35 {
            capsLock = true
            setUppercase(true)
        } else {
            setUppercase(!uppercase)
        }
        lastShift = now
    }

    @objc private func changePage() {
        if page == .letters { lastLetterLayout = keyLayout }
        page = page == .letters ? .numbers : .letters
        rebuild()
    }

    @objc private func changeSymbols() {
        page = page == .symbols ? .numbers : .symbols
        rebuild()
    }

    @objc private func deleteDown() { startDelete?() }
    @objc private func deleteUp() { endDelete?() }
}
