import CoreGraphics
import Foundation

/// Finds the dominant colour of a cover.
public enum ArtworkPalette {
    public static let sampleSize = 32

    private static let hueBuckets = 24
    private static let minimumAlpha: UInt8 = 128
    private static let nearBlack = 0.15
    private static let nearWhite = 0.90
    private static let minimumChroma = 0.15
    private static let minimumShare = 0.03

    /// `rgba` is premultiplied RGBA8, row-major, `width * height * 4` bytes; any other count returns nil.
    public static func dominant(rgba: [UInt8], width: Int, height: Int) -> RGB? {
        guard width > 0, height > 0, rgba.count == width * height * 4 else { return nil }
        let pixelCount = width * height

        var weight = [Double](repeating: 0, count: hueBuckets)
        var count = [Int](repeating: 0, count: hueBuckets)
        var sums = [(Double, Double, Double)](repeating: (0, 0, 0), count: hueBuckets)
        var qualifying = 0

        for index in 0..<pixelCount {
            let offset = index * 4
            let alpha = rgba[offset + 3]
            guard alpha >= minimumAlpha else { continue }
            let a = Double(alpha) / 255
            let r = min(1, Double(rgba[offset]) / 255 / a)
            let g = min(1, Double(rgba[offset + 1]) / 255 / a)
            let b = min(1, Double(rgba[offset + 2]) / 255 / a)
            let high = max(r, g, b)
            let low = min(r, g, b)
            let chroma = high - low
            guard high >= nearBlack, low <= nearWhite, chroma >= minimumChroma else { continue }

            let bucket = bucketIndex(red: r, green: g, blue: b, high: high, chroma: chroma)
            weight[bucket] += chroma
            count[bucket] += 1
            sums[bucket].0 += r
            sums[bucket].1 += g
            sums[bucket].2 += b
            qualifying += 1
        }

        guard Double(qualifying) >= max(1, minimumShare * Double(pixelCount)) else { return nil }
        // Score each pair of circularly adjacent buckets (a 30 degree window), so a cluster
        // that straddles a bucket edge is not split. Ties go to the lowest pair.
        func pairWeight(_ first: Int) -> Double { weight[first] + weight[(first + 1) % hueBuckets] }
        var winner = 0
        for first in 1..<hueBuckets where pairWeight(first) > pairWeight(winner) { winner = first }
        let second = (winner + 1) % hueBuckets
        let n = Double(count[winner] + count[second])
        return RGB(red: (sums[winner].0 + sums[second].0) / n,
                   green: (sums[winner].1 + sums[second].1) / n,
                   blue: (sums[winner].2 + sums[second].2) / n)
    }

    /// Draws `image` into a 32×32 sRGB premultiplied-last RGBA8 buffer, then calls `dominant(rgba:width:height:)`.
    public static func dominant(image: CGImage) -> RGB? {
        let size = sampleSize
        guard let space = CGColorSpace(name: CGColorSpace.sRGB) else { return nil }
        var bytes = [UInt8](repeating: 0, count: size * size * 4)
        let drawn = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard let context = CGContext(
                data: buffer.baseAddress, width: size, height: size, bitsPerComponent: 8,
                bytesPerRow: size * 4, space: space,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            context.interpolationQuality = .medium
            context.draw(image, in: CGRect(x: 0, y: 0, width: size, height: size))
            return true
        }
        guard drawn else { return nil }
        return dominant(rgba: bytes, width: size, height: size)
    }

    private static func bucketIndex(red: Double, green: Double, blue: Double,
                                    high: Double, chroma: Double) -> Int {
        var hue: Double
        if high == red {
            hue = (green - blue) / chroma
            if hue < 0 { hue += 6 }
        } else if high == green {
            hue = (blue - red) / chroma + 2
        } else {
            hue = (red - green) / chroma + 4
        }
        // hue is in [0, 6); scale to buckets.
        return min(hueBuckets - 1, Int(hue / 6 * Double(hueBuckets)))
    }
}
