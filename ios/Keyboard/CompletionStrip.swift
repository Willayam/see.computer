import UIKit

/// Correction and prediction buttons share their candidates with the space key.
final class CompletionStrip: UIStackView {
    let vocabulary = TypingVocabulary(store: TypingLanguages.store)
    private var trail = TapTrail()
    private var layout = KeyLayout()
    private var input: WordInput?
    private var shownInput: WordInput?
    private var swipeChoices: [String]?
    private var cached: WordSuggestions?
    private var canCorrect = false
    private var pending: DispatchWorkItem?
    /// Keeps a word clear of the dividers and of the capsule's rounded ends.
    private final class Choice: UIButton {
        override func layoutSubviews() {
            super.layoutSubviews()
            // Insetting a button narrower than the inset gives a null rectangle, which a layer rejects.
            if bounds.width > 8 { titleLabel?.frame = bounds.insetBy(dx: 4, dy: 0) }
        }
    }
    private let choices = (0..<3).map { _ in Choice(type: .custom) }
    /// Apple marks the word that space will accept with a pale capsule rather than a bolder face.
    private let capsules = (0..<3).map { _ in UIView() }
    private static let text = UIColor {
        $0.userInterfaceStyle == .dark ? UIColor(white: 162 / 255.0, alpha: 1) : UIColor(red: 74 / 255.0, green: 74 / 255.0, blue: 76 / 255.0, alpha: 1)
    }
    private static let capsule = UIColor {
        $0.userInterfaceStyle == .dark ? UIColor(white: 1, alpha: 0.1) : UIColor(red: 235 / 255.0, green: 237 / 255.0, blue: 240 / 255.0, alpha: 1)
    }
    /// The words sit this far below the middle of Apple's strip. The strip is moved down by
    /// it, and the capsules and dividers are moved back up.
    static let textDrop: CGFloat = 3
    var accept: ((WordInput, String) -> Void)?
    var shownTitles: [String] { choices.compactMap { $0.title(for: .normal) } }

    override init(frame: CGRect) {
        super.init(frame: frame)
        distribution = .fillEqually
        for (index, button) in choices.enumerated() {
            button.setTitleColor(Self.text, for: .normal)
            button.setTitleColor(Self.text.withAlphaComponent(0.5), for: .highlighted)
            button.titleLabel?.font = .systemFont(ofSize: 17)
            let capsule = capsules[index]
            capsule.backgroundColor = Self.capsule
            capsule.layer.cornerRadius = 22
            capsule.layer.cornerCurve = .continuous
            capsule.isUserInteractionEnabled = false
            capsule.isHidden = true
            capsule.translatesAutoresizingMaskIntoConstraints = false
            button.insertSubview(capsule, at: 0)
            NSLayoutConstraint.activate([
                capsule.leadingAnchor.constraint(equalTo: button.leadingAnchor, constant: 2),
                capsule.trailingAnchor.constraint(equalTo: button.trailingAnchor, constant: -2),
                capsule.topAnchor.constraint(equalTo: button.topAnchor, constant: -Self.textDrop),
                capsule.bottomAnchor.constraint(equalTo: button.bottomAnchor, constant: -Self.textDrop),
            ])
            // A third of the strip is narrower than most long words. Shrink them, and keep both
            // ends of a word that still does not fit, because completions differ at the end.
            button.titleLabel?.textAlignment = .center
            button.titleLabel?.adjustsFontSizeToFitWidth = true
            button.titleLabel?.minimumScaleFactor = 0.6
            button.titleLabel?.baselineAdjustment = .alignCenters
            button.titleLabel?.lineBreakMode = .byTruncatingMiddle
            button.addTarget(self, action: #selector(chosen(_:)), for: .touchUpInside)
            addArrangedSubview(button)
            if index > 0 {
                let divider = UIView()
                divider.backgroundColor = .separator
                divider.translatesAutoresizingMaskIntoConstraints = false
                button.addSubview(divider)
                NSLayoutConstraint.activate([
                    divider.leadingAnchor.constraint(equalTo: button.leadingAnchor),
                    divider.centerYAnchor.constraint(equalTo: button.centerYAnchor, constant: -Self.textDrop),
                    divider.heightAnchor.constraint(equalToConstant: 24),
                    divider.widthAnchor.constraint(equalToConstant: 0.5)
                ])
            }
        }
        vocabulary.load { [weak self] in self?.refresh() }
    }

    required init(coder: NSCoder) { fatalError("not used") }

    func configure(_ preference: TypingLanguages) {
        vocabulary.configure(preference, dictionaries: UITextChecker.availableLanguages)
        refresh()
    }

    func use(_ lexicon: UILexicon) {
        vocabulary.use(lexicon)
        refresh()
    }

    func update(for proxy: UITextDocumentProxy, trail: TapTrail, layout: KeyLayout, canCorrect: Bool, swipeChoices: [String]?) {
        let current = WordInput(proxy)
        let owned = trail.end != nil && trail.end == Caret(proxy) ? trail : TapTrail()
        if let shownInput,
           shownInput.field != (current?.field ?? SeeDocumentIdentifier(proxy)) ||
           (current == nil && (!WordInput.allowsCorrection(in: proxy) || proxy.selectedText?.isEmpty == false)) {
            setTitles(WordSuggestions(), for: nil)
        }
        guard self.swipeChoices != swipeChoices || input != current || self.canCorrect != canCorrect || self.trail != owned || self.layout != layout else { return }
        self.swipeChoices = swipeChoices
        self.trail = owned
        self.layout = layout
        self.canCorrect = canCorrect
        input = current
        refresh()
    }

    private func refresh() {
        cached = nil
        pending?.cancel()
        pending = nil
        let input = self.input
        if let input, let swipeChoices {
            setTitles(WordSuggestions(titles: swipeChoices), for: input)
            return
        }
        // Coalesce temporary context loss while the host is applying an edit.
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.input == input else { return }
            self.pending = nil
            let result = input.map { self.suggestions(for: $0, trail: self.trail, layout: self.layout) } ?? WordSuggestions()
            self.setTitles(result, for: input)
        }
        pending = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.035, execute: work)
    }

    func suggestions(for word: WordInput, trail: TapTrail, layout: KeyLayout) -> WordSuggestions {
        if input == word && self.trail == trail && self.layout == layout, let cached { return cached }
        let result = vocabulary.suggestions(for: word, trail: trail, layout: layout)
        if input == word && self.trail == trail && self.layout == layout { cached = result }
        return result
    }

    private func setTitles(_ result: WordSuggestions, for input: WordInput?) {
        shownInput = input
        for (index, button) in choices.enumerated() {
            let value = result.titles.indices.contains(index) ? result.titles[index] : nil
            let title = index == 0 && canCorrect && result.automatic != nil ? value.map { "“\($0)”" } : value
            button.setTitle(title, for: .normal)
            button.accessibilityLabel = value
            let proposed = canCorrect && value != nil && value == result.automatic
            capsules[index].isHidden = !proposed
            button.isEnabled = value != nil
            button.isAccessibilityElement = value != nil
        }
    }

    @objc private func chosen(_ button: UIButton) {
        guard pending == nil, let shownInput, shownInput == input, let candidate = button.accessibilityLabel else { return }
        accept?(shownInput, candidate)
    }
}
