import UIKit

/// Correction and prediction buttons share their candidates with the space key.
final class CompletionStrip: UIStackView {
    private let vocabulary = TypingVocabulary()
    private var input: WordInput?
    private var shownInput: WordInput?
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
        vocabulary.configure(preference)
        refresh()
    }

    func use(_ lexicon: UILexicon) {
        vocabulary.use(lexicon)
        refresh()
    }

    func update(for proxy: UITextDocumentProxy, canCorrect: Bool) {
        let current = WordInput(proxy)
        guard input != current || self.canCorrect != canCorrect else { return }
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
        // Keep the previous row visible during a fast key sequence. A tap is
        // validated against the field before its candidate can be applied.
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.input == input else { return }
            self.setTitles(self.suggestions(for: input))
        }
        pending = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.035, execute: work)
    }

    func suggestions(for word: WordInput) -> WordSuggestions {
        if input == word, let cached { return cached }
        let result = vocabulary.suggestions(for: word)
        if input == word { cached = result }
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
