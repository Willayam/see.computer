import UIKit

/// The letter rows stay in place while dictation runs. Only a page or language
/// change rebuilds them, so changing case never interrupts a touch.
final class TypingKeys: UIStackView {
    static let deleteRepeatDelay: TimeInterval = 0.45
    static let deleteRepeatInterval: TimeInterval = 0.09

    let preview = KeyPreview()
    private enum Page { case letters, numbers, symbols }
    private var page = Page.letters
    private var characterKeys: [Key] = []
    private var letterKeys: [Key] { page == .letters ? characterKeys : [] }
    private weak var spaceKey: Key?
    private weak var deleteKey: Key?
    private var shiftKey: Key?
    private let enter = Key(symbol: "return.left", label: "Return", output: "\n")
    private var returnType: UIReturnKeyType?
    private var returnEnabled = true
    private enum Field { case text, url, search }
    private var field = Field.text
    private(set) var uppercase = false
    private var capsLock = false
    private var lastShift = Date.distantPast
    private var swedish = false
    private let globe = Key(symbol: "globe", label: "Next keyboard")

    var keyPrior: ((KeyLayout) -> (probabilities: [Double], spread: Double, offsets: [Double])?)?
    var allowsSwipe: () -> Bool = { true }
    var onSwipe: (([SwipeSample], Bool, Bool, String?) -> Bool)?

    private weak var activeSwipe: Key?
    private var pendingReleases: [(origin: Key, key: Key, point: CGPoint)] = []

    func beginSwipe(from key: Key) -> Bool {
        guard activeSwipe == nil else { return false }
        activeSwipe = key
        return true
    }

    func endSwipe(from key: Key) {
        guard activeSwipe === key else { return }
        activeSwipe = nil
        let releases = pendingReleases
        pendingReleases.removeAll()
        for release in releases {
            activateRelease(from: release.origin, key: release.key, at: release.point)
        }
    }

    func canSwipe(from key: Key) -> Bool {
        activeSwipe == nil && page == .letters && letterKeys.contains(key) && onSwipe != nil && allowsSwipe()
    }

    struct StrokeTravel {
        private var neighbor: Key?
        private var crossedSeveralKeys = false
        private var reachedPitch = false

        mutating func isWordGesture(at point: CGPoint, start: CGPoint, from key: Key, on keyboard: TypingKeys) -> Bool {
            let cell = key.convert(key.bounds, to: keyboard)
            if hypot(point.x - start.x, point.y - start.y) >= cell.width { reachedPitch = true }
            if !cell.contains(point), let next = keyboard.releaseKey(at: point),
               next !== key, keyboard.letterKeys.contains(next) {
                // Sparse samples can jump over an intervening cell.
                let nextCell = next.convert(next.bounds, to: keyboard)
                if (neighbor != nil && neighbor !== next) || !cell.insetBy(dx: -1, dy: -1).intersects(nextCell) {
                    crossedSeveralKeys = true
                }
                neighbor = next
            }
            return reachedPitch && crossedSeveralKeys
        }
    }

    func finishSwipe(_ samples: [SwipeSample]) {
        lastShift = .distantPast
        _ = onSwipe?(samples, uppercase, capsLock, nil)
    }

    private static let commonPairs: Set<String> = [
        "am", "an", "as", "at", "be", "by", "do", "go", "he", "if", "in", "is", "it", "me", "my",
        "no", "of", "on", "or", "so", "to", "up", "us", "we", "av", "de", "du", "då", "en", "er",
        "ej", "få", "ge", "ha", "ja", "ju", "ni", "nu", "ny", "ok", "om", "på", "sa", "se", "så", "ta", "vi", "är", "åt", "öl"
    ]

    func finishShortSwipe(_ samples: [SwipeSample], from origin: Key) -> Bool {
        guard canSwipe(from: origin), let start = samples.first?.point, let point = samples.last?.point,
              let end = releaseKey(at: point), end !== origin, letterKeys.contains(end) else { return false }
        let cell = origin.convert(origin.bounds, to: self)
        let target = end.convert(end.bounds, to: self)
        guard !cell.contains(point), target.contains(point),
              hypot(point.x - start.x, point.y - start.y) >= cell.width * 0.6 else { return false }
        let pair = origin.output + end.output
        guard Self.commonPairs.contains(pair), onSwipe?(samples, uppercase, capsLock, pair) == true else { return false }
        lastShift = .distantPast
        return true
    }

    /// Replay the same start-key and travel checks used by touch delivery.
    func swipe(_ samples: [SwipeSample]) {
        guard let first = samples.first, let last = samples.last else { return }
        guard let origin = hitTest(first.point, with: nil) as? Key else { return }
        var travel = StrokeTravel()
        if canSwipe(from: origin), samples.dropFirst().contains(where: {
            travel.isWordGesture(at: $0.point, start: first.point, from: origin, on: self)
        }) {
            finishSwipe(samples)
        } else if finishShortSwipe(samples, from: origin) {
            return
        } else if resolvesAtRelease(origin) {
            let lastKey = samples.reversed().lazy.compactMap { self.releaseKey(at: $0.point) }.first
            release(from: origin, last: lastKey ?? origin, at: last.point)
        } else if origin === deleteKey {
            let held = max(0, last.time - first.time - Self.deleteRepeatDelay)
            let repeats = Int(held / Self.deleteRepeatInterval)
            for _ in 0...repeats { activate(origin, at: first.point) }
        } else if origin === hitTest(last.point, with: nil) {
            activate(origin, at: first.point)
        }
    }

    var moveCursor: ((Int) -> Void)?
    private var trackpadX: CGFloat = 0
    var insert: ((String, CGPoint?) -> Void)?
    var startDelete: (() -> Void)?
    var endDelete: (() -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        axis = .vertical
        distribution = .fill
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
        let kind: Field = proxy.keyboardType == .URL ? .url : proxy.keyboardType == .webSearch ? .search : .text
        if kind != field {
            field = kind
            rebuild()
        }
        let enabled = proxy.enablesReturnKeyAutomatically != true || proxy.hasText
        if proxy.returnKeyType != returnType || enabled != returnEnabled {
            returnEnabled = enabled
            returnType = proxy.returnKeyType
            let action: (String, String)
            switch returnType {
            case .search, .google, .yahoo: action = ("magnifyingglass", "Search")
            case .send: action = ("arrow.up", "Send")
            case .next: action = ("chevron.right", "Next")
            case .go, .continue: action = ("arrow.right", "Continue")
            case .done: action = ("checkmark", "Done")
            case .join: action = ("arrow.right", "Join")
            default: action = ("return.left", "Return")
            }
            enter.setSymbol(action.0)
            enter.accessibilityLabel = action.1
            enter.setActionColor(action.1 != "Return" && action.1 != "Next", enabled: enabled)
        }
        guard !capsLock else { return }
        let before = proxy.documentContextBeforeInput ?? ""
        let capitals = Typist.capitalizes(after: before, mode: proxy.autocapitalizationType)
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
        characterKeys = []
        shiftKey = nil
        var rows: [[String]]
        switch page {
        case .letters: rows = [swedish ? "qwertyuiopå" : "qwertyuiop", swedish ? "asdfghjklöä" : "asdfghjkl", "zxcvbnm"].map(Self.keys)
        case .numbers: rows = ["1234567890", "-/:;()", ".,?!’"].map(Self.keys)
            rows[1] += swedish ? ["kr", "&", "@", "”"] : ["$", "&", "@", "“"]
        case .symbols: rows = ["[]{}#%^*+=", "_\\|~<>€$£•", ".,?!’"].map(Self.keys)
        }
        for (index, titles) in rows.enumerated() {
            let controls = titles.map { typingKey($0) }
            let row = addRow()
            let cluster = UIStackView(arrangedSubviews: controls)
            cluster.distribution = .fillEqually
            let columns = page == .letters && swedish ? 11 : 10
            characterKeys += controls
            if index == 2 {
                let modifier: Key
                if page == .letters {
                    modifier = Key(symbol: "shift", label: "Shift")
                    shiftKey = modifier
                    modifier.addTarget(self, action: #selector(shift), for: .touchUpInside)
                } else {
                    modifier = Key(title: page == .numbers ? "#+=" : "123")
                    modifier.setTitleSize(page == .numbers ? 12 : 15)
                    modifier.addTarget(self, action: #selector(changeSymbols), for: .touchUpInside)
                }
                let delete = Key(symbol: "delete.left", pressedSymbol: "delete.left.fill", label: "Delete")
                deleteKey = delete
                delete.addTarget(self, action: #selector(deleteDown), for: .touchDown)
                delete.addTarget(self, action: #selector(deleteUp), for: [.touchUpInside, .touchUpOutside, .touchCancel])
                row.addArrangedSubview(modifier)
                row.addArrangedSubview(rowSpacer())
                row.addArrangedSubview(cluster)
                row.addArrangedSubview(rowSpacer())
                row.addArrangedSubview(delete)
                modifier.widthAnchor.constraint(equalTo: row.widthAnchor, multiplier: page == .letters && swedish ? 0.105 : 0.118, constant: 5).isActive = true
                delete.widthAnchor.constraint(equalTo: modifier.widthAnchor).isActive = true
            } else {
                row.addArrangedSubview(cluster)
            }
            cluster.centerXAnchor.constraint(equalTo: row.centerXAnchor).isActive = true
            if index < 2 && controls.count < columns {
                let left = rowSpacer()
                row.insertArrangedSubview(left, at: 0)
                row.addArrangedSubview(rowSpacer())
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
        language.font = .systemFont(ofSize: 13.5, weight: .regular, width: .compressed)
        language.textColor = UIColor { $0.userInterfaceStyle == .dark ? UIColor(white: 124 / 255.0, alpha: 1) : UIColor(white: 204 / 255.0, alpha: 1) }
        language.isAccessibilityElement = false
        language.translatesAutoresizingMaskIntoConstraints = false
        space.addSubview(language)
        NSLayoutConstraint.activate([
            language.trailingAnchor.constraint(equalTo: space.trailingAnchor, constant: -9),
            language.lastBaselineAnchor.constraint(equalTo: space.topAnchor, constant: Key.capInset + Key.capHeight - 6)
        ])
        let trackpad = UILongPressGestureRecognizer(target: self, action: #selector(trackCursor(_:)))
        trackpad.delaysTouchesEnded = false
        trackpad.minimumPressDuration = 0.35
        space.addGestureRecognizer(trackpad)
        let bottom = addRow()
        // Apple's URL keys replace the space bar, and web search adds a full stop beside it.
        let extras = (field == .url ? [".", "/", ".com"] : field == .search ? ["."] : []).map { typingKey($0, character: $0.count == 1) }
        let bottomKeys = [mode, globe] + (field == .url ? extras : [space] + extras) + [enter]
        bottomKeys.forEach(bottom.addArrangedSubview)
        for key in field == .url ? extras.dropFirst() : extras[...] {
            key.widthAnchor.constraint(equalTo: bottom.widthAnchor, multiplier: field == .url ? 0.1672 : 0.0997).isActive = true
        }
        for key in [mode, globe] {
            let width = key.widthAnchor.constraint(equalTo: bottom.widthAnchor, multiplier: 0.125)
            width.priority = .defaultHigh
            width.isActive = true
        }
        enter.widthAnchor.constraint(equalTo: bottom.widthAnchor, multiplier: field == .search ? 0.1757 : 0.25,
                                     constant: field == .search ? 0 : 1.0 / 3).isActive = true
        // Apple's rows are 54 pt apart. iOS ends a custom keyboard a few points above the bottom of
        // Apple's last row, so that row gets what is left and its caps come out shorter.
        for row in arrangedSubviews.dropLast() {
            let height = row.heightAnchor.constraint(equalToConstant: 54)
            height.priority = .init(999)
            height.isActive = true
        }
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
        let hit = super.hitTest(point, with: event)
        guard let key = hit as? Key else {
            guard let hit, hit !== preview, !hit.isDescendant(of: preview) else { return hit }
            let functionKeys = arrangedSubviews.compactMap { $0 as? UIStackView }
                .flatMap(\.arrangedSubviews).compactMap { $0 as? Key }.filter { !$0.isHidden }
            func distance(to key: Key) -> CGFloat {
                let frame = key.faceFrame(in: self)
                let dx = max(frame.minX - point.x, 0, point.x - frame.maxX)
                let dy = max(frame.minY - point.y, 0, point.y - frame.maxY)
                return dx * dx + dy * dy
            }
            return (characterKeys + functionKeys).min { distance(to: $0) < distance(to: $1) } ?? hit
        }
        let choice = resolved(key, at: point)
        return choice.hasActiveTouch ? key : choice
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
            // Space may compete across the row boundary. Other controls stay inside their cells.
            if $0 === spaceKey && isLetter {
                return point.x >= f.minX && point.x < f.maxX && point.y >= f.minY - rect.height * 0.2
            }
            guard $0 === key, f.contains(point) else { return false }
            if point.y <= f.minY + 8 { return true }
            if $0 === shiftKey { return point.x >= f.maxX - 8 }
            if $0 === deleteKey { return point.x <= f.minX + 8 }
            return false
        }
        guard isLetter || !nearby.isEmpty else { return key }
        let layout = keyLayout
        guard let prior = keyPrior?(layout) else { return key }
        let sigma = prior.spread * layout.pitch
        let scale = 0.5 / (sigma * sigma)
        func logPrior(_ i: Int) -> Double { log(0.95 * prior.probabilities[i] + 0.05 / 30) }
        var best = -Double.infinity
        var choice = key
        let atSpaceEdge = nearby.contains(where: { $0 === spaceKey })
        for letter in letterKeys {
            guard let ch = letter.output.first, let i = Lexicon.codes[ch], let c = layout.centers[i] else { continue }
            let boundaryBias = atSpaceEdge ? prior.offsets[i] * layout.pitch : 0
            let dx = point.x - c.x, dy = point.y - c.y - boundaryBias
            let cost = (dx * dx + dy * dy) * scale
            let score = logPrior(i) - cost
            if cost <= 10 && score > best { best = score; choice = letter }
        }
        for fn in nearby {
            let score: Double
            if fn === spaceKey {
                // Reaching into a letter cell requires evidence of a consistently high space tap.
                if isLetter && prior.offsets[30] > -0.06 { continue }
                let adjusted = CGPoint(x: point.x, y: point.y - prior.offsets[30] * layout.pitch)
                score = logPrior(30) - layout.spaceCost(adjusted, sigma: sigma)
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

    func resolvesAtRelease(_ key: Key) -> Bool {
        // Delete is chosen at touch down by hitTest, so a hold anywhere on it repeats.
        guard page == .letters, key !== deleteKey else { return false }
        return key === spaceKey || key === enter || key === shiftKey || letterKeys.contains(key)
    }

    func releaseKey(at point: CGPoint) -> Key? {
        guard let key = hitTest(point, with: nil) as? Key,
              key === deleteKey || resolvesAtRelease(key) else { return nil }
        return key
    }

    /// A touch that began on `origin`, was last over `last`, and ended at `point`.
    /// Sliding off shift onto a letter types it in capitals. A release that drifts off the keys
    /// types the letter the finger was last on, and return fires only from a touch that began on it.
    func release(from origin: Key, last: Key?, at point: CGPoint) {
        guard let key = releaseKey(at: point) ?? last.flatMap({ letterKeys.contains($0) ? $0 : nil }),
              key !== enter || origin === enter else { return }
        if activeSwipe != nil {
            pendingReleases.append((origin, key, point))
        } else {
            activateRelease(from: origin, key: key, at: point)
        }
    }

    private func activateRelease(from origin: Key, key: Key, at point: CGPoint) {
        if origin === shiftKey, key !== origin, letterKeys.contains(key), !uppercase { shift() }
        activate(key, at: point)
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

    private func rowSpacer() -> UIView {
        let spacer = UIView()
        // The remote keyboard host only forwards taps on rendered content.
        spacer.backgroundColor = Key.touchSurfaceColor
        return spacer
    }

    private func addRow() -> UIStackView {
        let row = UIStackView()
        addArrangedSubview(row)
        return row
    }

    private static func keys(_ characters: String) -> [String] { characters.map(String.init) }

    private func typingKey(_ title: String, character: Bool = true) -> Key {
        let key = Key(title: title, character: character)
        key.addTarget(self, action: #selector(typed(_:)), for: .touchUpInside)
        return key
    }

    @objc private func typed(_ key: Key) {
        if key === enter && !returnEnabled { return }
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
