import UIKit

/// Text fields that treat a keyboard's edits the way awkward host apps do, so a check can
/// compare what the keyboard believes with what the field really holds. The keyboard sees
/// its own guess of the field after a plain insert, and the host's answer only after
/// marked text, so neither the keyboard's log nor a screenshot of it is proof.
///
/// `Documents/command.txt` holds a query string such as `clear=1&set=plain&text=Hi&focus=plain`,
/// polled because a deep link asks for confirmation every time. `Documents/fields.tsv` holds
/// each field's focus, marked length and text, and `Documents/history.tsv` every edit a field
/// received, with the time.
class Field: UITextView {
    /// Shows our keyboard without the globe key, as the parity host does.
    override var textInputMode: UITextInputMode? {
        let ours = UITextInputMode.activeInputModes.first { String(describing: type(of: $0)).contains("Extension") }
        return ours ?? super.textInputMode
    }

    var markedLength: Int { markedTextRange.flatMap { text(in: $0) }?.count ?? 0 }
    var commitFirstCharacters: Int? { didSet { prefixCommitted = false } }
    private var prefixCommitted = false

    func note(_ call: String) {
        History.add("\(accessibilityIdentifier ?? "") \(call) -> len=\(text.count) marked=\(markedLength)")
    }

    override func insertText(_ text: String) {
        note("insert(\(text.count))")
        super.insertText(text)
    }

    // A keyboard extension's marked text arrives here, not through `setMarkedText`.
    override func setAttributedMarkedText(_ markedText: NSAttributedString?, selectedRange: NSRange) {
        note("mark(\(markedText?.length ?? 0))")
        if prefixCommitted { return }
        if let limit = commitFirstCharacters, limit > 0, let value = markedText?.string, value.count > limit {
            let prefix = NSAttributedString(string: String(value.prefix(limit)))
            super.setAttributedMarkedText(prefix, selectedRange: NSRange(location: prefix.length, length: 0))
            super.unmarkText()
            prefixCommitted = true
            try? text.write(to: History.documents.appendingPathComponent("partial.txt"), atomically: true, encoding: .utf8)
            return
        }
        super.setAttributedMarkedText(markedText, selectedRange: selectedRange)
    }

    override func unmarkText() {
        note("unmark")
        super.unmarkText()
    }
}

/// Shows no marked text, as a host with its own text engine can. Plain inserts land.
final class NoMarkField: Field {
    override func setAttributedMarkedText(_ markedText: NSAttributedString?, selectedRange: NSRange) {
        note("mark-ignored(\(markedText?.length ?? 0))")
    }

    override func unmarkText() { note("unmark-ignored") }
}

/// Takes no edit at all, as a read-only or busy field does. The keyboard cannot tell.
final class NoOpField: Field {
    override func insertText(_ text: String) { note("insert-ignored(\(text.count))") }
    override func deleteBackward() { note("delete-ignored") }

    override func setAttributedMarkedText(_ markedText: NSAttributedString?, selectedRange: NSRange) {
        note("mark-ignored(\(markedText?.length ?? 0))")
    }

    override func unmarkText() { note("unmark-ignored") }
}

enum History {
    static let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]

    static func add(_ line: String) {
        let url = documents.appendingPathComponent("history.tsv")
        let row = Data("\(Int(Date().timeIntervalSince1970 * 1000))\t\(line)\n".utf8)
        guard let handle = try? FileHandle(forWritingTo: url) else {
            try? row.write(to: url)
            return
        }
        handle.seekToEndOfFile()
        handle.write(row)
        try? handle.close()
    }
}

final class Host: UIViewController, UITextViewDelegate {
    private let fields: [(name: String, view: Field)] = [
        ("plain", Field()), ("nomark", NoMarkField()), ("noop", NoOpField()),
    ]
    /// An ordinary single-line field, which uses whichever keyboard was used last.
    private let single = UITextField()
    private var applied: String?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        let stack = UIStackView()
        stack.axis = .vertical
        stack.spacing = 4
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 4),
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -12),
        ])
        for (name, field) in fields {
            let label = UILabel()
            label.text = name
            label.font = .monospacedSystemFont(ofSize: 10, weight: .regular)
            label.textColor = .secondaryLabel
            field.font = .systemFont(ofSize: 14)
            field.layer.borderWidth = 1
            field.layer.borderColor = UIColor.separator.cgColor
            field.layer.cornerRadius = 6
            field.isScrollEnabled = false
            field.delegate = self
            field.accessibilityIdentifier = name
            field.heightAnchor.constraint(equalToConstant: 58).isActive = true
            stack.addArrangedSubview(label)
            stack.addArrangedSubview(field)
        }
        single.borderStyle = .roundedRect
        single.font = .systemFont(ofSize: 14)
        single.placeholder = "single"
        stack.addArrangedSubview(single)
        // A command left by an earlier launch was meant for that launch.
        applied = try? String(contentsOf: History.documents.appendingPathComponent("command.txt"), encoding: .utf8)
        Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [weak self] _ in self?.poll() }
    }

    func textViewDidChange(_ textView: UITextView) {
        (textView as? Field)?.note("changed")
        report()
    }

    private func field(_ name: String) -> Field? { fields.first { $0.name == name }?.view }

    private func report() {
        var rows = fields.map { name, field in
            let text = field.text.replacingOccurrences(of: "\n", with: "\\n")
            return "\(name)\t\(field.isFirstResponder ? 1 : 0)\t\(field.markedLength)\t\(text)"
        }
        rows.append("single\t\(single.isFirstResponder ? 1 : 0)\t0\t\(single.text ?? "")")
        let url = History.documents.appendingPathComponent("fields.tsv")
        try? rows.joined(separator: "\n").write(to: url, atomically: true, encoding: .utf8)
    }

    private func poll() {
        report()
        let url = History.documents.appendingPathComponent("command.txt")
        guard let command = try? String(contentsOf: url, encoding: .utf8), command != applied else { return }
        applied = command
        var query: [String: String] = [:]
        for item in URLComponents(string: "x:?" + command.trimmingCharacters(in: .whitespacesAndNewlines))?.queryItems ?? [] {
            query[item.name] = item.value ?? ""
        }
        if query["clear"] != nil {
            fields.forEach { $0.view.text = "" }
            single.text = ""
            field("plain")?.commitFirstCharacters = query["partial"].flatMap(Int.init)
        }
        if let name = query["set"], let text = query["text"] {
            if name == "single" { single.text = text }
            field(name)?.text = text
        }
        if let field = query["select"].flatMap(field) {
            field.selectedRange = NSRange(location: 0, length: (field.text as NSString).length)
        }
        if let field = query["caret"].flatMap(field) {
            field.selectedRange = NSRange(location: (field.text as NSString).length, length: 0)
        }
        if query["blur"] != nil || query["focus"] != nil { view.endEditing(true) }
        if let name = query["focus"] {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [self] in
                if name == "single" { single.becomeFirstResponder() }
                field(name)?.becomeFirstResponder()
            }
        }
        report()
    }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: scene)
        window.rootViewController = Host()
        window.makeKeyAndVisible()
        self.window = window
    }
}

final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let config = UISceneConfiguration(name: nil, sessionRole: session.role)
        config.delegateClass = SceneDelegate.self
        return config
    }
}

UIApplicationMain(CommandLine.argc, CommandLine.unsafeArgv, nil, NSStringFromClass(AppDelegate.self))
