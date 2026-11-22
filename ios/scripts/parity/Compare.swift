// Compares two full-screen simulator screenshots of the same keyboard state.
// Usage: compare <apple.png> <ours.png> <apple-snapshot.json> <out dir> <state> [tolerance]
// Writes <state>-side.png (Apple | ours | difference), <state>-caps.tsv, and appends a row to summary.tsv.
// Key caps are found in the pixels with the same rule for both keyboards, because the
// keyboard extension's views are not in the host's accessibility tree.
import AppKit
import CoreGraphics
import Foundation

struct Bitmap {
    let width: Int, height: Int
    var data: [UInt8]

    init(_ path: String) {
        guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else { fatalError("cannot read \(path)") }
        width = image.width
        height = image.height
        data = [UInt8](repeating: 0, count: width * height * 4)
        let context = CGContext(data: &data, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                                space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
    }

    init(width: Int, height: Int) {
        self.width = width
        self.height = height
        data = [UInt8](repeating: 255, count: width * height * 4)
    }

    func pixel(_ x: Int, _ y: Int) -> (Int, Int, Int) {
        let i = (y * width + x) * 4
        return (Int(data[i]), Int(data[i + 1]), Int(data[i + 2]))
    }

    mutating func set(_ x: Int, _ y: Int, _ c: (Int, Int, Int)) {
        let i = (y * width + x) * 4
        data[i] = UInt8(c.0); data[i + 1] = UInt8(c.1); data[i + 2] = UInt8(c.2); data[i + 3] = 255
    }

    func save(_ path: String) {
        var copy = data
        let context = CGContext(data: &copy, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                                space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        let rep = NSBitmapImageRep(cgImage: context.makeImage()!)
        try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))
    }
}

func distance(_ a: (Int, Int, Int), _ b: (Int, Int, Int)) -> Int {
    max(abs(a.0 - b.0), abs(a.1 - b.1), abs(a.2 - b.2))
}

func hex(_ c: (Int, Int, Int)) -> String { String(format: "#%02x%02x%02x", c.0, c.1, c.2) }

struct Cap {
    var x0: Int, y0: Int, x1: Int, y1: Int
    var label = ""
    var radius: Double = 0
    var face = (0, 0, 0)
    var ink: Double = 0
    var glyph = CGRect.null
    var cx: Double { Double(x0 + x1) / 2 }
    var cy: Double { Double(y0 + y1) / 2 }
}

let args = CommandLine.arguments
// compare changed <before.png> <after.png> <x> <y> <w> <h>: exit 0 when that area (points) changed.
if args.count == 8, args[1] == "changed" {
    let a = Bitmap(args[2]), b = Bitmap(args[3])
    let s = Double(a.width) / 402
    let r = args[4...7].map { Int((Double($0)! * s).rounded()) }
    var changed = 0
    for y in max(0, r[1])..<min(a.height, r[1] + r[3]) {
        for x in max(0, r[0])..<min(a.width, r[0] + r[2]) where distance(a.pixel(x, y), b.pixel(x, y)) > 12 { changed += 1 }
    }
    exit(changed > 20 ? 0 : 1)
}
guard args.count >= 6 else {
    print("usage: compare <apple.png> <ours.png> <apple-snapshot.json> <out dir> <state> [tolerance]")
    exit(2)
}
let apple = Bitmap(args[1]), ours = Bitmap(args[2])
let outDir = args[4], state = args[5]
let tolerance = args.count > 6 ? Int(args[6])! : 12
guard apple.width == ours.width, apple.height == ours.height else { fatalError("screenshots differ in size") }
let W = apple.width, H = apple.height
let scale = Double(W) / 402.0
func pt(_ px: Int) -> Double { (Double(px) / scale * 100).rounded() / 100 }
func px(_ p: Double) -> Int { Int((p * scale).rounded()) }

struct AXKey { let label: String; let rect: CGRect }
var axKeys: [AXKey] = []
var bands: [(String, Int, Int)] = []
if let data = FileManager.default.contents(atPath: args[3]),
   let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
   let nodes = (json["data"] as? [String: Any])?["nodes"] as? [[String: Any]] {
    for node in nodes {
        guard let r = node["rect"] as? [String: Double] else { continue }
        let rect = CGRect(x: r["x"]!, y: r["y"]!, width: r["width"]!, height: r["height"]!)
        let role = node["role"] as? String ?? ""
        if role == "UIAccessibilityElementKBKey", rect.width > 0 {
            axKeys.append(AXKey(label: node["label"] as? String ?? "?", rect: rect))
        }
        if role == "TUISystemInputAssistantView" { bands.append(("strip", px(rect.minY), px(rect.maxY))) }
        if role == "UIKeyboardLayoutStar" { bands.append(("keys", px(rect.minY), px(rect.maxY))) }
    }
}

/// The first row, scanning down from the middle of the screen, that is not the host's background.
func keyboardTop(_ image: Bitmap) -> Int {
    let start = H * 45 / 100
    let host = image.pixel(W / 8, start)
    for y in start..<H where [W / 8, W / 4, 3 * W / 4].allSatisfy({ distance(image.pixel($0, y), host) > 4 }) {
        return y
    }
    return H
}

let topA = keyboardTop(apple), topB = keyboardTop(ours)
let top = max(0, min(topA, topB) - px(4))
let regionH = H - top

var differing = 0
var bandCounts = [String: (Int, Int)]()
var diff = Bitmap(width: W, height: regionH)
for y in top..<H {
    for x in 0..<W {
        let a = apple.pixel(x, y), b = ours.pixel(x, y)
        let d = distance(a, b) > tolerance
        if d { differing += 1 }
        for (name, y0, y1) in bands where y >= y0 && y < y1 {
            var c = bandCounts[name] ?? (0, 0)
            c.1 += 1
            if d { c.0 += 1 }
            bandCounts[name] = c
        }
        let gray = (a.0 + a.1 + a.2) / 3
        let faint = 255 - (255 - gray) / 4
        diff.set(x, y - top, d ? (230, 20, 40) : (faint, faint, faint))
    }
}
let share = Double(differing) / Double(W * regionH)

/// Key caps: connected areas that differ from the keyboard's own background, sized like a key.
func caps(_ image: Bitmap, top: Int) -> (background: (Int, Int, Int), caps: [Cap]) {
    var samples: [(Int, Int, Int)] = []
    for y in stride(from: top + px(60), to: min(H - px(40), top + px(250)), by: 3) { samples.append(image.pixel(px(1.5), y)) }
    samples.sort { $0.0 + $0.1 + $0.2 < $1.0 + $1.1 + $1.2 }
    let background = samples.isEmpty ? image.pixel(px(1.5), H - 10) : samples[samples.count / 2]
    var mask = [Bool](repeating: false, count: W * H)
    for y in top..<H { for x in 0..<W where distance(image.pixel(x, y), background) > 14 { mask[y * W + x] = true } }
    var seen = [Bool](repeating: false, count: W * H)
    var found: [Cap] = []
    var stack: [Int] = []
    for start in (top * W)..<(W * H) where mask[start] && !seen[start] {
        var cap = Cap(x0: W, y0: H, x1: 0, y1: 0)
        stack.append(start)
        seen[start] = true
        while let i = stack.popLast() {
            let x = i % W, y = i / W
            cap.x0 = min(cap.x0, x); cap.x1 = max(cap.x1, x); cap.y0 = min(cap.y0, y); cap.y1 = max(cap.y1, y)
            for j in [x > 0 ? i - 1 : -1, x < W - 1 ? i + 1 : -1, y > top ? i - W : -1, y < H - 1 ? i + W : -1]
            where j >= 0 && mask[j] && !seen[j] {
                seen[j] = true
                stack.append(j)
            }
        }
        let w = pt(cap.x1 - cap.x0 + 1), h = pt(cap.y1 - cap.y0 + 1)
        guard w >= 15, h >= 28, h <= 70, w <= 300 else { continue }
        // Corner radius from where the top-left arc meets the diagonal.
        var t = 0
        while t < min(cap.x1 - cap.x0, cap.y1 - cap.y0) / 2 && !mask[(cap.y0 + t) * W + cap.x0 + t] { t += 1 }
        cap.radius = pt(t) / (1 - 1 / 2.squareRoot())
        cap.face = image.pixel(cap.x0 + px(5), cap.y0 + px(3))
        var ink = 0, area = 0
        for y in cap.y0...cap.y1 { for x in cap.x0...cap.x1 {
            area += 1
            if distance(image.pixel(x, y), cap.face) > 60 && mask[y * W + x] {
                ink += 1
                cap.glyph = cap.glyph.union(CGRect(x: x, y: y, width: 1, height: 1))
            }
        } }
        cap.ink = Double(ink) / Double(area)
        found.append(cap)
    }
    return (background, found)
}

var (bgA, capsA) = caps(apple, top: topA)
let (bgB, capsB) = caps(ours, top: topB)
for i in capsA.indices {
    let center = CGPoint(x: capsA[i].cx / scale, y: capsA[i].cy / scale)
    capsA[i].label = axKeys.first { $0.rect.contains(center) }?.label ?? "cap\(i)"
}

/// How far our label sits from Apple's, each measured from its own cap's top-left corner.
func glyphGap(_ a: Cap, _ b: Cap) -> [String] {
    guard !a.glyph.isNull, !b.glyph.isNull else { return ["-", "-", "-", "-"] }
    let dx = (b.glyph.minX - Double(b.x0)) - (a.glyph.minX - Double(a.x0))
    let dy = (b.glyph.minY - Double(b.y0)) - (a.glyph.minY - Double(a.y0))
    return [dx, dy, b.glyph.width - a.glyph.width, b.glyph.height - a.glyph.height].map { String(format: "%.2f", $0 / scale) }
}

// Pair each Apple cap with the nearest of ours that overlaps it, closest pairs first.
var candidates: [(Double, Int, Int)] = []
for (i, a) in capsA.enumerated() {
    for (j, b) in capsB.enumerated()
    where abs(a.cx - b.cx) < Double(a.x1 - a.x0) * 0.6 && abs(a.cy - b.cy) < Double(a.y1 - a.y0) * 0.6 {
        candidates.append((hypot(a.cx - b.cx, a.cy - b.cy), i, j))
    }
}
var pairOf = [Int: Int](), taken = Set<Int>()
for (_, i, j) in candidates.sorted(by: { $0.0 < $1.0 }) where pairOf[i] == nil && !taken.contains(j) {
    pairOf[i] = j
    taken.insert(j)
}

var lines = ["key\tapple_x\tapple_y\tapple_w\tapple_h\tours_x\tours_y\tours_w\tours_h\tgap\tapple_radius\tours_radius\tapple_face\tours_face\tink_ratio\tglyph_dx\tglyph_dy\tglyph_dw\tglyph_dh"]
var worst: (Double, String) = (0, "-")
var unmatched: [String] = []
func frame(_ c: Cap) -> String { "\(pt(c.x0))\t\(pt(c.y0))\t\(pt(c.x1 - c.x0 + 1))\t\(pt(c.y1 - c.y0 + 1))" }
for (i, cap) in capsA.enumerated().sorted(by: { ($0.1.cy, $0.1.cx) < ($1.1.cy, $1.1.cx) }) {
    guard let j = pairOf[i] else {
        unmatched.append("apple:\(cap.label)")
        lines.append("\(cap.label)\t\(frame(cap))\t-\t-\t-\t-\tmissing\t\(String(format: "%.1f", cap.radius))\t-\t\(hex(cap.face))\t-\t-")
        continue
    }
    let o = capsB[j]
    let gap = [pt(o.x0) - pt(cap.x0), pt(o.y0) - pt(cap.y0), pt(o.x1) - pt(cap.x1), pt(o.y1) - pt(cap.y1)].map(abs).max()!
    if gap > worst.0 { worst = (gap, cap.label) }
    lines.append(([
        cap.label, frame(cap), frame(o), String(format: "%.2f", gap), String(format: "%.1f", cap.radius), String(format: "%.1f", o.radius),
        hex(cap.face), hex(o.face), cap.ink > 0 ? String(format: "%.2f", o.ink / cap.ink) : "-",
    ] + glyphGap(cap, o)).joined(separator: "\t"))
}
for (j, o) in capsB.enumerated() where !taken.contains(j) {
    unmatched.append("ours@\(Int(pt(o.x0))),\(Int(pt(o.y0)))")
    lines.append("(ours only)\t-\t-\t-\t-\t\(frame(o))\textra\t-\t\(String(format: "%.1f", o.radius))\t-\t\(hex(o.face))\t-")
}
try! (lines.joined(separator: "\n") + "\n").write(toFile: "\(outDir)/\(state)-caps.tsv", atomically: true, encoding: .utf8)

// Apple | ours | difference, cropped to the keyboard.
let gutter = px(4)
var side = Bitmap(width: W * 3 + gutter * 2, height: regionH)
for y in 0..<regionH {
    for x in 0..<W {
        side.set(x, y, apple.pixel(x, y + top))
        side.set(x + W + gutter, y, ours.pixel(x, y + top))
        side.set(x + 2 * (W + gutter), y, diff.pixel(x, y))
    }
}
side.save("\(outDir)/\(state)-side.png")
diff.save("\(outDir)/\(state)-diff.png")

func bandShare(_ name: String) -> String {
    guard let c = bandCounts[name], c.1 > 0 else { return "-" }
    return String(format: "%.2f%%", Double(c.0) / Double(c.1) * 100)
}
let summary = "\(outDir)/summary.tsv"
if !FileManager.default.fileExists(atPath: summary) {
    try! "state\tdiff\tstrip_diff\tkeys_diff\tapple_top\tours_top\tapple_caps\tours_caps\tmax_gap_pt\tworst_key\tapple_bg\tours_bg\tunmatched\n"
        .write(toFile: summary, atomically: true, encoding: .utf8)
}
let row = [
    state, String(format: "%.2f%%", share * 100), bandShare("strip"), bandShare("keys"), "\(pt(topA))", "\(pt(topB))",
    "\(capsA.count)", "\(capsB.count)", String(format: "%.2f", worst.0), worst.1, hex(bgA), hex(bgB),
    unmatched.isEmpty ? "-" : unmatched.joined(separator: ","),
].joined(separator: "\t")
let handle = FileHandle(forWritingAtPath: summary)!
handle.seekToEndOfFile()
handle.write((row + "\n").data(using: .utf8)!)
print(row)
