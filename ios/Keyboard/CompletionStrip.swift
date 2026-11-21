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
    private let choices = (0..<3).map { _ in UIButton(type: .system) }
    var accept: ((WordInput, String) -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        distribution = .fillEqually
        for (index, button) in choices.enumerated() {
            button.tintColor = .label
            button.titleLabel?.font = .systemFont(ofSize: 16)
            button.titleLabel?.lineBreakMode = .byTruncatingTail
            button.addTarget(self, action: #selector(chosen(_:)), for: .touchUpInside)
            addArrangedSubview(button)
            if index > 0 {
                let divider = UIView()
                divider.backgroundColor = .separator
                divider.translatesAutoresizingMaskIntoConstraints = false
                button.addSubview(divider)
                NSLayoutConstraint.activate([
                    divider.leadingAnchor.constraint(equalTo: button.leadingAnchor),
                    divider.centerYAnchor.constraint(equalTo: button.centerYAnchor),
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
        guard let input else {
            setTitles(WordSuggestions())
            return
        }
        if let swipeChoices {
            setTitles(WordSuggestions(titles: swipeChoices))
            return
        }
        // Keep the previous row visible during a fast key sequence. A tap is
        // validated against the field before its candidate can be applied.
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.input == input else { return }
            self.setTitles(self.suggestions(for: input, trail: self.trail, layout: self.layout))
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

    private func setTitles(_ result: WordSuggestions) {
        shownInput = input
        for (index, button) in choices.enumerated() {
            let value = result.titles.indices.contains(index) ? result.titles[index] : nil
            let title = index == 0 && canCorrect && result.automatic != nil ? value.map { "“\($0)”" } : value
            button.setTitle(title, for: .normal)
            button.accessibilityLabel = value
            let proposed = canCorrect && value != nil && value == result.automatic
            button.titleLabel?.font = .systemFont(ofSize: 16, weight: proposed ? .semibold : .regular)
            button.isEnabled = value != nil
            button.isAccessibilityElement = value != nil
        }
    }

    @objc private func chosen(_ button: UIButton) {
        guard let shownInput, let candidate = button.accessibilityLabel else { return }
        accept?(shownInput, candidate)
    }
}
