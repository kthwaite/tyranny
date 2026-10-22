import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct ContrastTests {
    private func rgb(_ hex: String) -> RGB { RGB(hex: hex)! }

    private let samples = ["#0000FF", "#800080", "#FFFF00", "#00FF00", "#FF8000", "#808080"]

    private func backgrounds() -> [RGB] { [Contrast.lightBackground, Contrast.darkBackground] }

    @Test func blackWhiteIs21() {
        let black = rgb("#000000"), white = rgb("#FFFFFF")
        #expect(abs(Contrast.ratio(black, white) - 21) < 1e-9)
        #expect(Contrast.ratio(white, black) == Contrast.ratio(black, white))
        #expect(Contrast.luminance(white) == 1)
        #expect(Contrast.luminance(black) == 0)
        #expect(abs(Contrast.luminance(rgb("#808080")) - 0.21586) < 1e-4)
    }

    @Test func oklchKnownValueAndRoundTrip() throws {
        let red = OKLCH(rgb("#FF0000"))
        #expect(abs(red.l - 0.62796) < 1e-3)
        #expect(abs(red.c - 0.25768) < 1e-3)
        #expect(abs(red.h - 29.2339) < 1e-3)
        let original = rgb("#3366FF")
        let back = try #require(OKLCH(original).rgb())
        #expect(abs(back.red - original.red) < 1e-6)
        #expect(abs(back.green - original.green) < 1e-6)
        #expect(abs(back.blue - original.blue) < 1e-6)
    }

    @Test func raiseMeetsTheFloor() {
        for hex in samples {
            for background in backgrounds() {
                let raised = Contrast.raise(rgb(hex), against: background, to: 3)
                #expect(Contrast.ratio(raised, background) >= 3, "\(hex) against \(background.hex)")
            }
        }
    }

    @Test func raiseKeepsHue() {
        for hex in samples {
            let input = rgb(hex)
            for background in backgrounds() {
                let result = OKLCH(Contrast.raise(input, against: background, to: 3))
                guard result.c > 0.02 else { continue }
                let difference = abs(result.h - OKLCH(input).h)
                #expect(min(difference, 360 - difference) <= 3, "\(hex) against \(background.hex)")
            }
        }
    }

    @Test func raiseLeavesPassingColoursUnchanged() {
        let red = rgb("#FF0000")
        #expect(Contrast.raise(red, against: Contrast.darkBackground, to: 3) == red)
    }

    @Test func compositeAndTextChoice() {
        let black = rgb("#000000"), white = rgb("#FFFFFF")
        let mixed = Contrast.composite(black, opacity: 0.45, over: white)
        #expect(abs(mixed.red - 0.55) < 1e-9)
        #expect(abs(mixed.green - 0.55) < 1e-9)
        #expect(abs(mixed.blue - 0.55) < 1e-9)
        #expect(Contrast.prefersLightText(on: black))
        #expect(Contrast.prefersLightText(on: rgb("#1A4D99")))
        #expect(!Contrast.prefersLightText(on: white))
        #expect(!Contrast.prefersLightText(on: rgb("#FFE066")))
    }

    @Test func warningNamesTheFailingAppearance() {
        #expect(Contrast.warning(for: rgb("#FFFF00")) == "Low contrast in Light appearance")
        #expect(Contrast.warning(for: rgb("#000080")) == "Low contrast in Dark appearance")
        #expect(Contrast.warning(for: rgb("#808080")) == nil)
    }
}
