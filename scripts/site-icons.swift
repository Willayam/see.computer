// Builds the website's icons from the app's Icon Composer file, so the site shows the same icon as the app.
// Usage: swift scripts/site-icons.swift <path to the .icon folder> <site folder>
// Writes icon.png (light), icon-dark.png and apple-touch-icon.png into the site folder.

import CoreGraphics
import Foundation
import ImageIO

let arguments = CommandLine.arguments
guard arguments.count == 3 else {
    FileHandle.standardError.write(Data("usage: site-icons.swift <.icon folder> <site folder>\n".utf8))
    exit(2)
}
let layers = URL(fileURLWithPath: arguments[1]).appendingPathComponent("Assets")
let site = URL(fileURLWithPath: arguments[2])

func layer(_ name: String) -> CGImage {
    let url = layers.appendingPathComponent(name)
    guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
          image.width == 1024, image.height == 1024
    else {
        FileHandle.standardError.write(Data("cannot read a 1024 px layer at \(url.path)\n".utf8))
        exit(1)
    }
    return image
}

// Each layer fills its 1024 px square and macOS cuts the corners. The site's icons get the same
// corners here. iOS cuts its own, so the home-screen icon stays square.
let cornerRadius = 0.2237

func write(_ file: String, appearance: String, rounded: Bool, size: Int) {
    let context = CGContext(
        data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    )!
    context.interpolationQuality = .high
    let whole = CGRect(x: 0, y: 0, width: size, height: size)
    if rounded {
        let radius = CGFloat(size) * cornerRadius
        context.addPath(CGPath(roundedRect: whole, cornerWidth: radius, cornerHeight: radius, transform: nil))
        context.clip()
    }
    for name in ["key", "art"] {
        context.draw(layer("\(name)-\(appearance).png"), in: whole)
    }
    let url = site.appendingPathComponent(file)
    guard let image = context.makeImage(),
          let destination = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil)
    else { exit(1) }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else { exit(1) }
    print(url.path)
}

write("icon.png", appearance: "light", rounded: true, size: 128)
write("icon-dark.png", appearance: "dark", rounded: true, size: 128)
write("apple-touch-icon.png", appearance: "light", rounded: false, size: 180)
