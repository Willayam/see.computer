import UIKit

enum TypingLanguages: String, CaseIterable, Identifiable {
    case device, english, swedish, bilingual

    var id: Self { self }

    var label: String {
        switch self {
        case .device: "Use iPhone languages"
        case .english: "English"
        case .swedish: "Svenska"
        case .bilingual: "English & Svenska"
        }
    }

    @MainActor var usesSwedishKeys: Bool {
        switch self {
        case .device:
            let keyboards = UITextInputMode.activeInputModes.compactMap(\.primaryLanguage)
            return (Locale.preferredLanguages + keyboards).contains {
                Locale(identifier: $0).language.languageCode?.identifier == "sv"
            }
        case .english: return false
        case .swedish, .bilingual: return true
        }
    }

    static let key = "typingLanguages"
    static let neuralSwipeKey = "newSwipeModel"
    static let store: UserDefaults? = {
        let defaults = UserDefaults(suiteName: Bridge.group)
        if defaults?.object(forKey: neuralSwipeKey) == nil,
           let disabled = defaults?.object(forKey: "disableNeuralSwipe") as? Bool {
            defaults?.set(!disabled, forKey: neuralSwipeKey)
        }
        return defaults
    }()

    static var current: Self {
        store?.string(forKey: key).flatMap(Self.init(rawValue:)) ?? .device
    }
}
