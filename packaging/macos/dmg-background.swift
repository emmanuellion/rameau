// Background of the Rameau disk image window (660×400 pt, normal and Retina).
// Regenerate: swift packaging/macos/dmg-background.swift packaging/macos
// The icons are placed over it by packaging/macos/dmg-settings.py: app at (165, 190),
// Applications at (495, 190), from the top-left corner.
import AppKit

let (w, h): (CGFloat, CGFloat) = (660, 400)
let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "."

func color(_ hex: UInt32, _ alpha: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat(hex >> 16 & 0xff) / 255, green: CGFloat(hex >> 8 & 0xff) / 255, blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
}

// Points measured from the top, like the icon positions.
func pt(_ x: CGFloat, _ y: CGFloat) -> NSPoint { NSPoint(x: x, y: h - y) }

func draw() {
    // Soft vertical gradient.
    NSGradient(starting: color(0xfbfcfe), ending: color(0xeceff5))!.draw(in: NSRect(x: 0, y: 0, width: w, height: h), angle: -90)

    // Two small branches of the logo, faint, in opposite corners: a lane, a branch leaving it, their commits.
    func branch(x: CGFloat, top: CGFloat, bottom: CGFloat, to: (CGFloat, CGFloat), main: UInt32, side: UInt32) {
        let stroke = { (hex: UInt32, path: NSBezierPath) in
            path.lineWidth = 6
            path.lineCapStyle = .round
            color(hex, 0.22).setStroke()
            path.stroke()
        }
        let lane = NSBezierPath()
        lane.move(to: pt(x, top))
        lane.line(to: pt(x, bottom))
        stroke(main, lane)
        let fork = NSBezierPath()
        let start = (x, (top + bottom) / 2)
        let mid = (start.1 + to.1) / 2
        fork.move(to: pt(start.0, start.1))
        fork.curve(to: pt(to.0, to.1), controlPoint1: pt(start.0, mid), controlPoint2: pt(to.0, mid))
        stroke(side, fork)
        for (cx, cy, hex) in [(x, top, main), (x, bottom, main), (to.0, to.1, side)] {
            color(hex, 0.32).setFill()
            NSBezierPath(ovalIn: NSRect(x: cx - 7, y: h - cy - 7, width: 14, height: 14)).fill()
        }
    }
    branch(x: 46, top: 250, bottom: 370, to: (90, 230), main: 0x5ab0f6, side: 0xf0719b)
    branch(x: 614, top: 30, bottom: 150, to: (570, 10), main: 0x7bd88f, side: 0xb18cf5)

    // Arrow from the app to Applications.
    let arrow = NSBezierPath()
    arrow.lineWidth = 4
    arrow.lineCapStyle = .round
    arrow.setLineDash([10, 9], count: 2, phase: 0)
    arrow.move(to: pt(262, 190))
    arrow.line(to: pt(388, 190))
    color(0x2f7fd1, 0.85).setStroke()
    arrow.stroke()
    let head = NSBezierPath()
    head.lineWidth = 4
    head.lineCapStyle = .round
    head.lineJoinStyle = .round
    head.move(to: pt(380, 176))
    head.line(to: pt(398, 190))
    head.line(to: pt(380, 204))
    head.stroke()

    // Instructions under the icons and their names.
    func text(_ s: String, size: CGFloat, weight: NSFont.Weight, hex: UInt32, y: CGFloat) {
        let style = NSMutableParagraphStyle()
        style.alignment = .center
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: size, weight: weight),
            .foregroundColor: color(hex),
            .paragraphStyle: style,
        ]
        NSAttributedString(string: s, attributes: attrs).draw(in: NSRect(x: 0, y: h - y - size * 1.4, width: w, height: size * 1.6))
    }
    text("Glisse Rameau dans le dossier Applications", size: 16, weight: .semibold, hex: 0x2b313d, y: 318)
    text("puis lance-le depuis le Launchpad ou Spotlight", size: 12.5, weight: .regular, hex: 0x7a8291, y: 346)
}

for (scale, name) in [(1, "background.png"), (2, "background@2x.png")] {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(w) * scale, pixelsHigh: Int(h) * scale, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    rep.size = NSSize(width: w, height: h)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    draw()
    NSGraphicsContext.restoreGraphicsState()
    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "\(out)/\(name)"))
}
