import UIKit

/// One plain text field configured by `Documents/field.txt`, a query string such as
/// kb=apple-sv&return=go&type=email&enables=1&text=Hi, so Apple's keyboard and ours
/// can be shown in the same field. The file is polled, because a deep link asks for
/// confirmation every time.
final class Field: UITextField {
    var wanted = "ours"

    override var textInputMode: UITextInputMode? {
        let modes = UITextInputMode.activeInputModes
        let match: UITextInputMode?
        switch wanted {
        case "apple-sv": match = modes.first { $0.primaryLanguage == "sv-SE" && !Self.isExtension($0) }
        case "apple-en": match = modes.first { $0.primaryLanguage == "en-US" && !Self.isExtension($0) }
        default: match = modes.first(where: Self.isExtension)
        }
        return match ?? super.textInputMode
    }

    static func isExtension(_ mode: UITextInputMode) -> Bool {
        String(describing: type(of: mode)).contains("Extension")
    }
}

final class Host: UIViewController {
    static var allowsCustomKeyboards = true
    let field = Field()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        field.borderStyle = .roundedRect
        field.translatesAutoresizingMaskIntoConstraints = false
        field.accessibilityIdentifier = "parity-field"
        view.addSubview(field)
        NSLayoutConstraint.activate([
            field.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 40),
            field.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 20),
            field.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -20),
            field.heightAnchor.constraint(equalToConstant: 44),
        ])
    }

    static var documents: URL { FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0] }
    private var applied: String?

    func poll() {
        let text = (try? String(contentsOf: Self.documents.appendingPathComponent("field.txt"), encoding: .utf8)) ?? ""
        guard text != applied else { return }
        applied = text
        apply(text.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    func apply(_ settings: String) {
        var query: [String: String] = [:]
        for item in URLComponents(string: "x:?" + settings)?.queryItems ?? [] {
            query[item.name] = item.value ?? ""
        }
        field.resignFirstResponder()
        field.wanted = query["kb"] ?? "ours"
        let returns: [String: UIReturnKeyType] = [
            "default": .default, "go": .go, "google": .google, "join": .join, "next": .next, "route": .route,
            "search": .search, "send": .send, "yahoo": .yahoo, "done": .done, "emergency": .emergencyCall,
            "continue": .continue,
        ]
        field.returnKeyType = returns[query["return"] ?? "default"] ?? .default
        let types: [String: UIKeyboardType] = [
            "default": .default, "email": .emailAddress, "url": .URL, "websearch": .webSearch,
            "numberpad": .numberPad, "twitter": .twitter, "ascii": .asciiCapable,
        ]
        field.keyboardType = types[query["type"] ?? "default"] ?? .default
        field.enablesReturnKeyAutomatically = query["enables"] == "1"
        field.autocorrectionType = query["autocorrect"] == "0" ? .no : .default
        field.autocapitalizationType = query["cap"] == "none" ? .none : .sentences
        field.text = query["text"] ?? ""
        let modes = UITextInputMode.activeInputModes.map { "\(type(of: $0)) \($0.primaryLanguage ?? "-")" }
        try? modes.joined(separator: "\n").write(to: Self.documents.appendingPathComponent("modes.txt"), atomically: true, encoding: .utf8)
        Host.allowsCustomKeyboards = field.wanted == "ours"
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { self.field.becomeFirstResponder() }
    }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    let host = Host()

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        self.window = window
        Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [host] _ in host.poll() }
    }
}

final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let config = UISceneConfiguration(name: nil, sessionRole: session.role)
        config.delegateClass = SceneDelegate.self
        return config
    }

    func application(_ application: UIApplication,
                     shouldAllowExtensionPointIdentifier extensionPointIdentifier: UIApplication.ExtensionPointIdentifier) -> Bool {
        extensionPointIdentifier != .keyboard || Host.allowsCustomKeyboards
    }
}

UIApplicationMain(CommandLine.argc, CommandLine.unsafeArgv, nil, NSStringFromClass(AppDelegate.self))
