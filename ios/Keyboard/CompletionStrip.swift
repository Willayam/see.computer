import UIKit

/// A completion adds only the missing suffix. Selecting or moving within a
/// word leaves editing to the host's text selection menu.
final class CompletionStrip: UIStackView {
    private let checker = UITextChecker()
    private var prefix = ""
    private var needsRefresh = true
    private var languages: [String] = []
    private let choices = (0..<3).map { _ in UIButton(type: .system) }
    var accept: ((String, String) -> Void)?

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
    }

    required init(coder: NSCoder) { fatalError("not used") }

    func configure(_ preference: TypingLanguages) {
        let swedish = preference.usesSwedishKeys
        let available = UITextChecker.availableLanguages
        let codes = preference == .swedish ? ["sv"] : (swedish ? ["sv", "en"] : ["en"])
        languages = codes.compactMap { code in
            available.first { Locale(identifier: $0).language.languageCode?.identifier == code }
        }
        needsRefresh = true
    }

    static func word(in proxy: UITextDocumentProxy) -> String {
        guard proxy.selectedText?.isEmpty != false, proxy.autocorrectionType != .no,
            proxy.documentContextAfterInput?.first?.isLetter != true,
            let before = proxy.documentContextBeforeInput else { return "" }
        return String(before.reversed().prefix(while: { $0.isLetter }).reversed())
    }

    func update(for proxy: UITextDocumentProxy) {
        let word = Self.word(in: proxy)
        guard word != prefix || needsRefresh else { return }
        prefix = word
        needsRefresh = false
        var titles = [String]()
        if !word.isEmpty {
            titles.append(word)
            let range = NSRange(word.startIndex..., in: word)
            let lists = languages.map { checker.completions(forPartialWordRange: range, in: word, language: $0) ?? [] }
            for rank in 0..<3 {
                for list in lists where list.indices.contains(rank) {
                    let candidate = list[rank]
                    if candidate.hasPrefix(word), !titles.contains(candidate) { titles.append(candidate) }
                }
            }
        }
        for (index, button) in choices.enumerated() {
            let title = titles.indices.contains(index) ? titles[index] : nil
            button.setTitle(title, for: .normal)
            button.isEnabled = title != nil
            button.isAccessibilityElement = title != nil
        }
    }

    @objc private func chosen(_ button: UIButton) {
        guard let title = button.currentTitle else { return }
        accept?(prefix, String(title.dropFirst(prefix.count)))
    }
}
