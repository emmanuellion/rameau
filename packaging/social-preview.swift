// Social preview of Rameau (1280×640): GitHub Settings › Social preview, and og:image of the site.
// Regenerate: swift packaging/social-preview.swift assets/screenshots/dark.png assets/social-preview.png
import AppKit

let (w, h): (CGFloat, CGFloat) = (1280, 640)
let args = CommandLine.arguments
let shot = NSImage(contentsOfFile: args[1])!
let icon = NSImage(contentsOfFile: "assets/icon.png")!

func color(_ hex: UInt32, _ alpha: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat(hex >> 16 & 0xff) / 255, green: CGFloat(hex >> 8 & 0xff) / 255, blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
}

func text(_ s: String, size: CGFloat, weight: NSFont.Weight, hex: UInt32, x: CGFloat, top: CGFloat) {
    let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: size, weight: weight), .foregroundColor: color(hex)]
    NSAttributedString(string: s, attributes: attrs).draw(at: NSPoint(x: x, y: h - top - size * 1.2))
}

let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(w), pixelsHigh: Int(h), bitsPerSample: 8, samplesPerPixel: 4,
                           hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

color(0x14161b).setFill()
NSRect(x: 0, y: 0, width: w, height: h).fill()

// Screenshot, rounded, bleeding off the right and bottom edges.
let frame = NSRect(x: 560, y: -120, width: 900, height: 900 * shot.size.height / shot.size.width)
NSGraphicsContext.saveGraphicsState()
NSBezierPath(roundedRect: frame, xRadius: 14, yRadius: 14).addClip()
shot.draw(in: frame)
NSGraphicsContext.restoreGraphicsState()
color(0x262a33).setStroke()
NSBezierPath(roundedRect: frame, xRadius: 14, yRadius: 14).stroke()

// Title block.
icon.draw(in: NSRect(x: 72, y: h - 96 - 96, width: 96, height: 96))
text("Rameau", size: 72, weight: .bold, hex: 0xe4e7ee, x: 72, top: 224)
text("A fast, native Git client", size: 30, weight: .medium, hex: 0xe4e7ee, x: 72, top: 330)
text("written in Rust with GPUI", size: 30, weight: .medium, hex: 0x8a90a0, x: 72, top: 372)
text("macOS, Linux and Windows", size: 22, weight: .regular, hex: 0x5ab0f6, x: 72, top: 450)
text("github.com/emmanuellion/rameau", size: 20, weight: .regular, hex: 0x5c6270, x: 72, top: 560)

NSGraphicsContext.current = nil
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: args[2]))
