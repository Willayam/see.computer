import Foundation

/// One line per event in `Library/diagnostics.tsv` in the App Group folder, from
/// both the app and the keyboard, so a take that went wrong leaves a trace. It holds
/// sizes, times and levels, never the words.
enum Diagnostics {
    /// The Mac's press trail empties itself at the same size.
    private static let cap: off_t = 1 << 20

    /// Under `Library`, because `devicectl` refuses to copy files from the folder's top level.
    private static let path: String? = FileManager.default
        .containerURL(forSecurityApplicationGroupIdentifier: Bridge.group)?
        .appendingPathComponent("Library/diagnostics.tsv").path

    private static let source = Bundle.main.bundlePath.hasSuffix(".appex") ? "keyboard" : "app"

    static func log(_ event: String, _ detail: KeyValuePairs<String, Any> = [:]) {
        guard let path else { return }
        let fields = detail.map { "\($0.key)=\(format($0.value))" }.joined(separator: " ")
        let millis = Int64(Date().timeIntervalSince1970 * 1000)
        let line = "\(millis)\t\(source)\t\(event)\t\(fields)\n"
        // Both processes append to this file, so each line goes out in one write
        // to a descriptor opened for appending.
        let descriptor = open(path, O_WRONLY | O_APPEND | O_CREAT, 0o644)
        guard descriptor >= 0 else { return }
        defer { close(descriptor) }
        var info = stat()
        if fstat(descriptor, &info) == 0, info.st_size > cap { ftruncate(descriptor, 0) }
        var bytes = Array(line.utf8)
        _ = write(descriptor, &bytes, bytes.count)
    }

    private static func format(_ value: Any) -> String {
        let text: String
        switch value {
        case let number as Double: text = String(format: "%.3f", number)
        case let number as Float: text = String(format: "%.4f", number)
        case let flag as Bool: text = flag ? "yes" : "no"
        default: text = "\(value)"
        }
        // Spaces separate the pairs and tabs the columns, so an error message keeps neither.
        return String(text.map { $0.isWhitespace ? "_" : $0 })
    }
}
