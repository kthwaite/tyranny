import Foundation

/// WCAG 2 contrast maths and a hue-preserving lightness raise.
public enum Contrast {
    public static let floor = 3.0
    public static let lightBackground = RGB(red: 0xEC / 255, green: 0xEC / 255, blue: 0xEC / 255)
    public static let darkBackground = RGB(red: 0x1E / 255, green: 0x1E / 255, blue: 0x1E / 255)

    private static let black = RGB(red: 0, green: 0, blue: 0)
    private static let white = RGB(red: 1, green: 1, blue: 1)

    /// WCAG 2 relative luminance.
    public static func luminance(_ colour: RGB) -> Double {
        0.2126 * linear(colour.red) + 0.7152 * linear(colour.green) + 0.0722 * linear(colour.blue)
    }

    /// (lighter + 0.05) / (darker + 0.05). Symmetric.
    public static func ratio(_ a: RGB, _ b: RGB) -> Double {
        let la = luminance(a), lb = luminance(b)
        return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)
    }

    /// Moves the colour's OKLCH lightness, keeping hue, until it meets `ratio`
    /// against `background`. A colour that already meets it is returned unchanged.
    public static func raise(_ colour: RGB, against background: RGB, to ratio: Double) -> RGB {
        if self.ratio(colour, background) >= ratio { return colour }
        let darken = self.ratio(black, background) >= self.ratio(white, background)
        let start = OKLCH(colour)
        let endpoint = darken ? 0.0 : 1.0

        func candidate(at lightness: Double) -> RGB? {
            let full = OKLCH(l: lightness, c: start.c, h: start.h)
            if let direct = full.rgb() { return direct }
            var low = 0.0, high = start.c
            var best = OKLCH(l: lightness, c: 0, h: start.h).rgb()
            for _ in 0..<20 {
                let mid = (low + high) / 2
                if let found = OKLCH(l: lightness, c: mid, h: start.h).rgb() {
                    best = found
                    low = mid
                } else {
                    high = mid
                }
            }
            return best
        }

        guard var best = candidate(at: endpoint), self.ratio(best, background) >= ratio else {
            return darken ? black : white
        }
        var near = start.l, far = endpoint
        for _ in 0..<32 {
            let mid = (near + far) / 2
            if let found = candidate(at: mid), self.ratio(found, background) >= ratio {
                best = found
                far = mid
            } else {
                near = mid
            }
        }
        return best
    }

    /// Per component `c * opacity + background * (1 - opacity)`.
    public static func composite(_ colour: RGB, opacity: Double, over background: RGB) -> RGB {
        RGB(red: colour.red * opacity + background.red * (1 - opacity),
            green: colour.green * opacity + background.green * (1 - opacity),
            blue: colour.blue * opacity + background.blue * (1 - opacity))
    }

    public static func prefersLightText(on fill: RGB) -> Bool {
        luminance(fill) < 0.4
    }

    public static func warning(for colour: RGB) -> String? {
        if ratio(colour, lightBackground) < floor { return "Low contrast in Light appearance" }
        if ratio(colour, darkBackground) < floor { return "Low contrast in Dark appearance" }
        return nil
    }

    private static func linear(_ c: Double) -> Double {
        c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4)
    }
}

/// OKLCH: Ottosson's OKLab in polar form. `h` is in degrees, 0..<360.
struct OKLCH: Equatable {
    var l: Double
    var c: Double
    var h: Double

    init(l: Double, c: Double, h: Double) {
        self.l = l
        self.c = c
        self.h = h
    }

    init(_ rgb: RGB) {
        func lin(_ v: Double) -> Double { v <= 0.04045 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4) }
        let r = lin(rgb.red), g = lin(rgb.green), b = lin(rgb.blue)
        let l_ = cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b)
        let m_ = cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b)
        let s_ = cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b)
        let lab = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_
        let a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_
        let bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_
        var hue = atan2(bb, a) * 180 / .pi
        if hue < 0 { hue += 360 }
        if hue >= 360 { hue -= 360 }
        self.init(l: lab, c: (a * a + bb * bb).squareRoot(), h: hue)
    }

    /// nil when the colour is outside the sRGB gamut.
    func rgb() -> RGB? {
        let radians = h * .pi / 180
        let a = c * cos(radians), b = c * sin(radians)
        let l_ = l + 0.3963377774 * a + 0.2158037573 * b
        let m_ = l - 0.1055613458 * a - 0.0638541728 * b
        let s_ = l - 0.0894841775 * a - 1.2914855480 * b
        let lc = l_ * l_ * l_, mc = m_ * m_ * m_, sc = s_ * s_ * s_
        let r = 4.0767416621 * lc - 3.3077115913 * mc + 0.2309699292 * sc
        let g = -1.2684380046 * lc + 2.6097574011 * mc - 0.3413193965 * sc
        let bl = -0.0041960863 * lc - 0.7034186147 * mc + 1.7076147010 * sc
        let epsilon = 1e-6
        for v in [r, g, bl] where !(v >= -epsilon && v <= 1 + epsilon) { return nil }
        func encode(_ v: Double) -> Double {
            let x = min(max(v, 0), 1)
            return x <= 0.0031308 ? x * 12.92 : 1.055 * pow(x, 1 / 2.4) - 0.055
        }
        return RGB(red: encode(r), green: encode(g), blue: encode(bl))
    }
}
