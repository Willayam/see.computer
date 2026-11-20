import UIKit

/// The letter rows stay in place while dictation runs. Only a page or language
/// change rebuilds them, so changing case never interrupts a touch.
final class TypingKeys: UIStackView {
    let preview = KeyPreview()
    private enum Page { case letters, numbers, symbols }
    private var page = Page.letters
    private var letterKeys: [Key] = []
    private var shiftKey: Key?
    private let enter = Key(symbol: "return.left", label: "Return", output: "\n")
    private var returnType: UIReturnKeyType?
    private var uppercase = false
    private var capsLock = false
    private var lastShift = Date.distantPast
    private var swedish = false
    private let globe = Key(symbol: "globe", label: "Next keyboard")

    var moveCursor: ((Int) -> Void)?
    private var trackpadX: CGFloat = 0
    var insert: ((String) -> Void)?
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
        insert?(value)
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
