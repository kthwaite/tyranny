import CoreGraphics
import Testing
@testable import TyrannyCore

@Suite struct ArtworkPaletteTests {
    typealias Pixel = (UInt8, UInt8, UInt8)

    private func buffer(_ pixel: (Int, Int) -> Pixel) -> [UInt8] {
        var bytes: [UInt8] = []
        for y in 0..<32 {
            for x in 0..<32 {
                let (r, g, b) = pixel(x, y)
                bytes += [r, g, b, 255]
            }
        }
        return bytes
    }

    private func expectEqual(_ actual: RGB?, _ expected: RGB, tolerance: Double = 1e-9) {
        guard let actual else {
            Issue.record("expected \(expected), got nil")
            return
        }
        #expect(abs(actual.red - expected.red) <= tolerance)
        #expect(abs(actual.green - expected.green) <= tolerance)
        #expect(abs(actual.blue - expected.blue) <= tolerance)
    }

    @Test func solidRedReturnsRed() {
        let rgba = buffer { _, _ in (255, 0, 0) }
        expectEqual(ArtworkPalette.dominant(rgba: rgba, width: 32, height: 32),
                    RGB(red: 1, green: 0, blue: 0))
    }

    @Test func greyReturnsNil() {
        let grey = buffer { _, _ in (0x80, 0x80, 0x80) }
        let black = buffer { _, _ in (0, 0, 0) }
        #expect(ArtworkPalette.dominant(rgba: grey, width: 32, height: 32) == nil)
        #expect(ArtworkPalette.dominant(rgba: black, width: 32, height: 32) == nil)
    }

    @Test func greyWithAFewColouredPixelsReturnsNil() {
        let rgba = buffer { x, y in y == 0 && x < 10 ? (255, 0, 0) : (0x80, 0x80, 0x80) }
        #expect(ArtworkPalette.dominant(rgba: rgba, width: 32, height: 32) == nil)
    }

    @Test func seventyPercentBlueBeatsGrey() {
        let rgba = buffer { _, y in y < 22 ? (0, 0, 255) : (0x80, 0x80, 0x80) }
        expectEqual(ArtworkPalette.dominant(rgba: rgba, width: 32, height: 32),
                    RGB(red: 0, green: 0, blue: 1))
    }

    @Test func smallSaturatedAreaOnBlackWins() {
        let rgba = buffer { x, y in x < 8 && y < 8 ? (0xFF, 0x80, 0x00) : (0, 0, 0) }
        expectEqual(ArtworkPalette.dominant(rgba: rgba, width: 32, height: 32),
                    RGB(hex: "#FF8000")!)
    }

    @Test func wrongBufferSizeReturnsNil() {
        #expect(ArtworkPalette.dominant(rgba: [UInt8](repeating: 255, count: 10), width: 32, height: 32) == nil)
        #expect(ArtworkPalette.dominant(rgba: [], width: 0, height: 0) == nil)
    }

    @Test func cgImagePathSamplesTheImage() throws {
        let space = try #require(CGColorSpace(name: CGColorSpace.sRGB))
        let context = try #require(CGContext(
            data: nil, width: 64, height: 64, bitsPerComponent: 8, bytesPerRow: 0,
            space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        context.setFillColor(CGColor(colorSpace: space, components: [0, 0, 1, 1])!)
        context.fill(CGRect(x: 0, y: 0, width: 64, height: 64))
        let image = try #require(context.makeImage())
        expectEqual(ArtworkPalette.dominant(image: image), RGB(red: 0, green: 0, blue: 1),
                    tolerance: 1.0 / 255)
    }
}
