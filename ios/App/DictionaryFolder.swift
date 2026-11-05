import Foundation

/// The Mac keeps `vocabulary.md` and `learned.md` in `~/Documents/see.computer`,
/// which iCloud Drive also shows here. The user picks that folder once. Each time
/// the app comes forward it copies both files to where the engine reads them.
enum DictionaryFolder {
    private static let bookmarkKey = "dictionaryFolder"
    private static let names = ["vocabulary.md", "learned.md"]

    static var isConnected: Bool {
        UserDefaults.standard.data(forKey: bookmarkKey) != nil
    }

    static func connect(_ url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        guard let bookmark = try? url.bookmarkData() else { return }
        UserDefaults.standard.set(bookmark, forKey: bookmarkKey)
    }

    static func disconnect() {
        UserDefaults.standard.removeObject(forKey: bookmarkKey)
    }

    /// Runs off the main thread, because reading an iCloud file can wait on its download.
    static func sync() {
        guard let bookmark = UserDefaults.standard.data(forKey: bookmarkKey) else { return }
        var stale = false
        guard let folder = try? URL(resolvingBookmarkData: bookmark, bookmarkDataIsStale: &stale) else {
            return
        }
        let scoped = folder.startAccessingSecurityScopedResource()
        defer { if scoped { folder.stopAccessingSecurityScopedResource() } }
        if stale, let fresh = try? folder.bookmarkData() {
            UserDefaults.standard.set(fresh, forKey: bookmarkKey)
        }
        try? FileManager.default.createDirectory(at: Paths.dictionary, withIntermediateDirectories: true)
        for name in names {
            copy(folder.appendingPathComponent(name), to: Paths.dictionary.appendingPathComponent(name))
        }
    }

    /// The whole file or nothing, so the engine never reads half a dictionary.
    private static func copy(_ source: URL, to destination: URL) {
        var failure: NSError?
        NSFileCoordinator().coordinate(readingItemAt: source, options: [], error: &failure) { url in
            guard let data = try? Data(contentsOf: url), data != (try? Data(contentsOf: destination))
            else { return }
            try? data.write(to: destination, options: .atomic)
        }
    }

    /// How many words the engine will read, for the settings row.
    static func termCount() -> Int {
        names.reduce(0) { total, name in
            let url = Paths.dictionary.appendingPathComponent(name)
            guard let text = try? String(contentsOf: url, encoding: .utf8) else { return total }
            return total + text.split(separator: "\n").filter { $0.hasPrefix("- ") }.count
        }
    }
}
