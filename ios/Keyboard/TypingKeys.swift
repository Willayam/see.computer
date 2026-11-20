import UIKit

/// The letter rows stay in place while dictation runs. Only a page or language
/// change rebuilds them, so changing case never interrupts a touch.
final class TypingKeys: UIStackView {
    private enum Page { case letters, numbers, symbols }
    private var page = Page.letters
    private var letterKeys: [Key] = []
    private var shiftKey: Key?
    private var uppercase = false
    private var capsLock = false
    private var lastShift = Date.distantPast
    private var swedish = false
    private let globe = Key(symbol: "globe", label: "Next keyboard")

    var insert: ((String) -> Void)?
    var startDelete: (() -> Void)?
    var endDelete: (() -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        axis = .vertical
        distribution = .fillEqually
        spacing = 6
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
        setUppercase(capitals)
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
            var controls = characters.map { typingKey(String($0)) }
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
                controls.insert(modifier, at: 0)
                controls.append(delete)
            }
            let row = addRow(controls)
            row.distribution = .fillEqually
        }
        let mode = Key(title: page == .letters ? "123" : "ABC")
        mode.addTarget(self, action: #selector(changePage), for: .touchUpInside)
        let space = typingKey("space", output: " ")
        let enter = Key(symbol: "return.left", label: "Return", output: "\n")
        enter.addTarget(self, action: #selector(typed(_:)), for: .touchUpInside)
        let bottom = addRow([mode, globe, space, typingKey("."), enter])
        for (key, fraction) in [(mode, 0.15), (enter, 0.17)] {
            key.widthAnchor.constraint(equalTo: bottom.widthAnchor, multiplier: fraction).isActive = true
        }
        bottom.arrangedSubviews[3].widthAnchor.constraint(equalTo: bottom.widthAnchor, multiplier: 0.10).isActive = true
        let globeWidth = globe.widthAnchor.constraint(equalTo: bottom.widthAnchor, multiplier: 0.11)
        globeWidth.priority = .defaultHigh
        globeWidth.isActive = true
        setUppercase(uppercase)
    }

    private func addRow(_ controls: [Key]) -> UIStackView {
        let row = UIStackView(arrangedSubviews: controls)
        row.spacing = 4
        addArrangedSubview(row)
        return row
    }

    private func typingKey(_ title: String, output: String? = nil) -> Key {
        let key = Key(title: title, output: output)
        key.addTarget(self, action: #selector(typed(_:)), for: .touchUpInside)
        return key
    }

    @objc private func typed(_ key: Key) {
        lastShift = .distantPast
        let value = page == .letters && uppercase ? key.output.uppercased() : key.output
        insert?(value)
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
