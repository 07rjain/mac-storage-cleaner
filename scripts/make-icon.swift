// Draws the app icon, a small sunburst on a dark rounded square, as a 1024 × 1024 PNG.
// Usage: swift scripts/make-icon.swift <output.png>

import AppKit

let size: CGFloat = 1024
let output = CommandLine.arguments.dropFirst().first ?? "AppIcon.png"

let space = CGColorSpace(name: CGColorSpace.sRGB)!
let context = CGContext(
    data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8, bytesPerRow: 0,
    space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!

func color(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(
        srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
        blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
}

// Apple's icon grid: an 824-point rounded square centered in the 1024 canvas.
let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
let tilePath = CGPath(roundedRect: tile, cornerWidth: 185, cornerHeight: 185, transform: nil)
context.addPath(tilePath)
context.clip()
let gradient = CGGradient(
    colorsSpace: space, colors: [color(0x2c2c30), color(0x141416)] as CFArray,
    locations: [0, 1])!
context.drawLinearGradient(
    gradient, start: CGPoint(x: 0, y: tile.maxY), end: CGPoint(x: 0, y: tile.minY), options: [])

let center = CGPoint(x: size / 2, y: size / 2)

/// One ring segment from `start` to `end`, in turns clockwise from the top.
func arc(inner: CGFloat, outer: CGFloat, start: CGFloat, end: CGFloat, fill: CGColor) {
    let gap: CGFloat = 0.004
    let from = CGFloat.pi / 2 - (start + gap) * 2 * .pi
    let to = CGFloat.pi / 2 - (end - gap) * 2 * .pi
    let path = CGMutablePath()
    path.addArc(center: center, radius: outer, startAngle: from, endAngle: to, clockwise: true)
    path.addArc(center: center, radius: inner, startAngle: to, endAngle: from, clockwise: false)
    path.closeSubpath()
    context.addPath(path)
    context.setFillColor(fill)
    context.fillPath()
}

let blue: UInt32 = 0x0a84ff
let green: UInt32 = 0x30d158
let orange: UInt32 = 0xff9f0a
let pink: UInt32 = 0xff375f
let teal: UInt32 = 0x64d2ff

// Inner ring: the top-level folders.
let inner: [(CGFloat, CGFloat, UInt32)] = [
    (0.00, 0.46, blue), (0.46, 0.68, green), (0.68, 0.84, orange), (0.84, 0.94, pink),
    (0.94, 1.00, teal),
]
for (start, end, hex) in inner {
    arc(inner: 150, outer: 250, start: start, end: end, fill: color(hex))
}

// Outer ring: their children, lighter, leaving some space unfilled.
let outer: [(CGFloat, CGFloat, UInt32)] = [
    (0.00, 0.22, blue), (0.22, 0.34, blue), (0.34, 0.42, blue), (0.46, 0.60, green),
    (0.60, 0.66, green), (0.68, 0.79, orange), (0.84, 0.91, pink),
]
for (start, end, hex) in outer {
    arc(inner: 262, outer: 340, start: start, end: end, fill: color(hex, 0.62))
}

context.setFillColor(color(0x2c2c2e))
context.fillEllipse(in: CGRect(x: center.x - 138, y: center.y - 138, width: 276, height: 276))

let image = context.makeImage()!
let rep = NSBitmapImageRep(cgImage: image)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: output))
