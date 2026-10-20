import Testing
@testable import TyrannyCore

@Suite struct StationFileTests {
    @Test func plsParsesEntriesInNumericOrder() throws {
        let text = """
            [playlist]
            NumberOfEntries=3
            File10=http://ten.example/stream
            Title10=Ten
            File2=https://two.example/stream
            Title2=Two
            File1=http://one.example/stream
            Title1=One
            Version=2
            """
        let stations = try StationFile.parse(text, fileName: "mix.pls")
        #expect(stations == [
            StationInput(name: "One", url: "http://one.example/stream"),
            StationInput(name: "Two", url: "https://two.example/stream"),
            StationInput(name: "Ten", url: "http://ten.example/stream"),
        ])
    }

    @Test func plsToleratesRealWorldVariants() throws {
        let text = "\u{FEFF}[playlist]\r\nfile1=http://a.example/s\r\nTITLE1= Alpha \r\nFile3= http://c.example/s \r\n"
        let stations = try StationFile.parse(text, fileName: "MIX.PLS")
        #expect(stations == [
            StationInput(name: "Alpha", url: "http://a.example/s"),
            StationInput(name: "", url: "http://c.example/s"),
        ])
    }

    @Test func m3uReadsExtinfTitles() throws {
        let text = """
            #EXTM3U
            #EXTINF:-1,First Radio
            http://first.example/live
            #EXTVLCOPT:network-caching=1000
            https://untitled.example/live
            #EXTINF:-1,Third, With Comma
            HTTP://third.example/live
            """
        let stations = try StationFile.parse(text, fileName: "radio.m3u8")
        #expect(stations == [
            StationInput(name: "First Radio", url: "http://first.example/live"),
            StationInput(name: "", url: "https://untitled.example/live"),
            StationInput(name: "Third, With Comma", url: "HTTP://third.example/live"),
        ])
    }

    @Test func m3uSkipsNonHTTPAndRejectsEmpty() {
        let text = "#EXTM3U\n#EXTINF:1,Local\n/Users/me/a.flac\nrtsp://x.example/live\nmusic/b.mp3\n"
        #expect(throws: StationFileError.self) { try StationFile.parse(text, fileName: "local.m3u") }
        do {
            _ = try StationFile.parse(text, fileName: "local.m3u")
        } catch let error as StationFileError {
            #expect(error.message == "local.m3u has no http or https stations")
        } catch {
            Issue.record("unexpected error \(error)")
        }
    }

    @Test func duplicateURLsInOneFileKeepFirst() throws {
        let text = "#EXTINF:-1,Keep\nhttp://a.example/s\n#EXTINF:-1,Drop\nhttp://a.example/s\nhttp://b.example/s\n"
        let stations = try StationFile.parse(text, fileName: "d.m3u")
        #expect(stations == [
            StationInput(name: "Keep", url: "http://a.example/s"),
            StationInput(name: "", url: "http://b.example/s"),
        ])
    }

    @Test func unknownExtensionThrows() {
        #expect(throws: StationFileError.self) { try StationFile.parse("http://a.example/s", fileName: "a.txt") }
        #expect(throws: StationFileError.self) { try StationFile.parse("http://a.example/s", fileName: "noextension") }
    }
}
