import Foundation
import KeyboardCore
import KeyboardEnglish

final class Document: TextDocument {
  var text = ""
  var textBeforeInput: String? { text }
  var textAfterInput: String? { nil }
  func insert(_ value: String) { text += value }
  func deleteBackward(_ count: Int) { text.removeLast(min(count, text.count)) }
  func moveCursor(by offset: Int) {}
}

struct Sentence: Decodable {
  let person, id: String
  let taps: [[[Double]]]
  let spaces: [[Double]?]
}

@main enum Race {
  @MainActor static func main() throws {
    let args = CommandLine.arguments
    func read(_ path: String) throws -> Data { try Data(contentsOf: URL(fileURLWithPath: path)) }
    let frames = try JSONDecoder().decode([String: [Double]].self, from: read(args[1]))
    let swedish = frames["å"] != nil
    let letters =
      swedish ? ["qwertyuiopå", "asdfghjklöä", "zxcvbnm"] : ["qwertyuiop", "asdfghjkl", "zxcvbnm"]
    let labels = letters.map { $0.map(String.init) } + [["Space"]]
    let rows: [[RowItem]] = labels.map { row in
      var items = [RowItem]()
      var edge = 0.0
      for k in row {
        let f = frames[k]!
        let left = f[0] - f[2] / 2
        if left - edge > 0.01 { items.append(.spacer(.pitch(left - edge))) }
        items.append(.key(Key(k == "Space" ? .space : .character(k), width: .pitch(f[2]))))
        edge = left + f[2]
      }
      return items
    }
    let grid = KeyGrid(
      KeyboardLayout(rows: rows), width: 394,
      metrics: GridMetrics(
        columns: 394, sideInset: 0, keyGap: 0, rowGap: 0, keyHeight: 52.5, numberRowHeight: 52.5,
        topInset: 0))
    let source =
      swedish ? Lexicon(bytes: Bytes.mapped(URL(fileURLWithPath: args[4]))!)! : Lexicon.english
    var corrector: Corrector!
    var person = ""
    var results = [[String: Any]]()
    for s in try JSONDecoder().decode([Sentence].self, from: read(args[2])) {
      if person != s.person {
        person = s.person
        corrector = Corrector(source: source, network: swedish ? nil : .english)
        corrector.grid = grid
      }
      var engine = TypingEngine(traits: InputTraits(autocapitalization: .none))
      let doc = Document()
      engine.corrector = corrector
      engine.suggestionsChanged()
      var commits = [String]()
      var bars = [[String]]()
      for (index, points) in s.taps.enumerated() {
        if args.contains("--reset-touches") { corrector.changeMemory { $0.touches = TouchModel() } }
        let before = doc.text
        func tap(_ p: [Double]) {
          guard let hit = engine.key(atX: p[0], y: p[1], in: grid),
            let label = grid.rows[hit.row][hit.key].key.label
          else { return }
          engine.perform(
            label == " " ? .space : .character(label), in: doc,
            tap: Tap(x: p[0], y: p[1], key: label))
        }
        points.forEach { tap($0) }
        bars.append(engine.suggestions.prefix(3).map(\.text))
        if args.contains("--observed-spaces"), let p = s.spaces[index] {
          tap(p)
        } else {
          engine.perform(.space, in: doc)
        }
        commits.append(
          doc.text.hasPrefix(before)
            ? String(doc.text.dropFirst(before.count))
            : String(doc.text.split(separator: " ").last ?? ""))
      }
      results.append(["id": s.id, "text": doc.text, "bars": bars, "commits": commits])
      engine.endSession()
    }
    try JSONSerialization.data(withJSONObject: results).write(to: URL(fileURLWithPath: args[3]))
  }
}
