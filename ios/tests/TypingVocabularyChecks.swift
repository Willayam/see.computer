import UIKit

/// Runs against the installed simulator dictionaries and the exact keyboard word tables.
@MainActor
private final class Field: NSObject, UITextDocumentProxy {
    var documentContextBeforeInput: String?
    var documentContextAfterInput: String?
    var selectedText: String?
    var documentInputMode: UITextInputMode? { nil }
    let documentIdentifier = UUID()
    var autocorrectionType = UITextAutocorrectionType.default
    var keyboardType = UIKeyboardType.default
    var hasText: Bool { documentContextBeforeInput?.isEmpty == false }
    func insertText(_ text: String) { preconditionFailure("Suggestions must not edit the field") }
    func deleteBackward() { preconditionFailure("Suggestions must not edit the field") }
    func adjustTextPosition(byCharacterOffset offset: Int) {}
    func setMarkedText(_ markedText: String, selectedRange: NSRange) {}
    func unmarkText() {}
}

@main
enum TypingVocabularyChecks {
    @MainActor static func main() async {
        let vocabulary = TypingVocabulary()
        vocabulary.configure(.bilingual, dictionaries: UITextChecker.availableLanguages)
        await withCheckedContinuation { continuation in vocabulary.load { continuation.resume() } }
        let field = Field()
        precondition(SeeDocumentIdentifier(field) == field.documentIdentifier)
        precondition(SeeDocumentIdentifier(UIInputViewController().textDocumentProxy) == nil)
        func suggestions(_ text: String) -> WordSuggestions {
            field.documentContextBeforeInput = text
            return WordInput(field).map(vocabulary.suggestions) ?? WordSuggestions()
        }
        for (input, expected) in [("Teh", "The"), ("We need helpp", "help"), ("Jag skriver itne", "inte"),
                                  ("Det är mycekt", "mycket"), ("Det är braa", "bra")] {
            precondition(suggestions(input).automatic == expected, "Correction for \(input)")
        }
        for input in ["We need jag", "Vi ses tomorrow", "Send it to Jonh", "Flag XPC", "Use GitHbu"] {
            precondition(suggestions(input).automatic == nil, "Preserve \(input)")
        }
        let partial = suggestions("I will go tomo")
        precondition(partial.automatic == nil && partial.titles.contains("tomorrow"))
        precondition(suggestions("Vi ses imorg").titles.contains("imorgon"))
        precondition(suggestions("Jag ").titles.contains("har"))
        precondition(suggestions("We ").titles.contains("are"))
        for input in ["user@teh", "example.teh", "some_teh", "#teh"] {
            precondition(suggestions(input).titles.isEmpty, "Leave identifier \(input) alone")
        }
        field.selectedText = "here"
        precondition(suggestions("teh").titles.isEmpty)
        field.selectedText = nil
        field.documentContextAfterInput = "ing"
        precondition(suggestions("teh").titles.isEmpty)
        field.documentContextAfterInput = nil
        field.autocorrectionType = .no
        precondition(suggestions("teh").titles.isEmpty)
        field.autocorrectionType = .default
        field.keyboardType = .URL
        precondition(suggestions("teh").titles.isEmpty)
        field.keyboardType = .default
        vocabulary.configure(.bilingual, dictionaries: ["en_US"])
        precondition(suggestions("Vi ses imorg").titles.contains("imorgon"))
        precondition(suggestions("Jag ").titles.contains("har"))
        precondition(suggestions("Vi ses itne").automatic == nil)
        print("English and Swedish typing checks passed")
    }
}
