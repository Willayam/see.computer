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
    private let choices = (0..<3).map { _ in UIButton(type: .custom) }
    var accept: ((WordInput, String) -> Void)?
    var shownTitles: [String] { choices.compactMap { $0.title(for: .normal) } }

    override init(frame: CGRect) {
        super.init(frame: frame)
        distribution = .fillEqually
        for (index, button) in choices.enumerated() {
            button.setTitleColor(.label, for: .normal)
            button.setTitleColor(UIColor.label.withAlphaComponent(0.5), for: .highlighted)
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
            button.titleLabel?.font = .systemFont(ofSize: 16, weight: proposed ? .semibold : .regular)
            button.isEnabled = value != nil
            button.isAccessibilityElement = value != nil
        }
    }

    @objc private func chosen(_ button: UIButton) {
        guard pending == nil, let shownInput, shownInput == input, let candidate = button.accessibilityLabel else { return }
        accept?(shownInput, candidate)
    }
}
