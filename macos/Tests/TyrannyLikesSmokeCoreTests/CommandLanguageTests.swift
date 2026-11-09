import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct CommandLanguageTests {
    @Test func parsesEverySemanticActionWithItsArguments() throws {
        let cases: [(String, CommandID, [String])] = [
            ("play", .play, []), ("pause", .pause, []), ("toggle", .toggle, []),
            ("stop", .stop, []), ("next", .next, []), ("previous", .previous, []),
            ("seek -1.5m", .seek, ["-1.5m"]), ("volume +5%", .volume, ["+5%"]),
            ("repeat cycle", .repeatMode, ["cycle"]), ("shuffle toggle", .shuffle, ["toggle"]),
            ("gain album", .gain, ["album"]), ("resume on", .resume, ["on"]),
            ("go up-next", .go, ["up-next"]), ("filter ''", .filter, [""]),
            ("focus-filter", .focusFilter, []), ("reveal-playing", .revealPlaying, []),
            ("browser off", .browser, ["off"]), ("drawer on", .drawer, ["on"]),
            ("artwork toggle", .artwork, ["toggle"]), ("settings playback", .settings, ["playback"]),
            ("colour accent #3366ff", .colour, ["accent", "#3366ff"]), ("colours reset", .colours, ["reset"]), ("tint off", .tint, ["off"]),
            ("settings appearance", .settings, ["appearance"]), ("settings artwork", .settings, ["artwork"]),
            ("settings keys", .settings, ["keys"]), ("equalizer", .equalizer, []), ("lyrics toggle", .lyrics, ["toggle"]),
            ("connect", .connect, []), ("disconnect", .disconnect, []),
            ("server restart", .server, ["restart"]), ("choose-folder", .chooseFolder, []),
            ("rescan 18446744073709551615", .rescan, ["18446744073709551615"]),
            ("retry-import", .retryImport, []), ("browse-library", .browseLibrary, []), ("quit", .quit, []),
            ("playlist-open 'Road Trip'", .playlistOpen, ["Road Trip"]),
            ("playlist-create 'New List'", .playlistCreate, ["New List"]),
            ("playlist-rename 'Road Trip' 'Evening Music'", .playlistRename, ["Road Trip", "Evening Music"]),
            ("playlist-delete 'Road Trip'", .playlistDelete, ["Road Trip"]),
            ("playlist-play 'Road Trip'", .playlistPlay, ["Road Trip"]),
            ("playlist-enqueue 'Road Trip'", .playlistEnqueue, ["Road Trip"]),
            ("playlist-add 'Road Trip'", .playlistAdd, ["Road Trip"]),
            ("smart-create Jazz 'genre:Jazz year:2020'", .smartCreate, ["Jazz", "genre:Jazz year:2020"]),
            ("smart-update Jazz 'genre:Jazz'", .smartUpdate, ["Jazz", "genre:Jazz"]),
            ("smart-snapshot Jazz 'Jazz Today'", .smartSnapshot, ["Jazz", "Jazz Today"]),
            ("queue-clear", .queueClear, []), ("queue-shuffle", .queueShuffle, []),
            ("queue-save 'Current Queue'", .queueSave, ["Current Queue"]),
            ("selection-play", .selectionPlay, []), ("selection-run", .selectionRun, []),
            ("selection-next", .selectionNext, []), ("selection-enqueue", .selectionEnqueue, []),
            ("selection-remove", .selectionRemove, []), ("selection-album", .selectionAlbum, []),
            ("selection-artist", .selectionArtist, []), ("selection-all", .selectionAll, []),
            ("selection-clear", .selectionClear, []), ("selection-move end", .selectionMove, ["end"]),
            ("group artist_album", .group, ["artist_album"]),
            ("sort album_artist desc", .sort, ["album_artist", "desc"]),
            ("column album-artist toggle", .column, ["album-artist", "toggle"]),
            ("album-sort date_added", .albumSort, ["date_added"]),
            ("cover-size 128.5", .coverSize, ["128.5"]),
            ("album-expand", .albumExpand, []), ("album-collapse", .albumCollapse, []),
            ("musicbrainz", .musicbrainz, []),
            ("browser-genre 42", .browserGenre, ["42"]), ("browser-artist all", .browserArtist, ["all"]),
            ("browser-album 9", .browserAlbum, ["9"]),
            ("up-next-policy replace", .upNextPolicy, ["replace"]),
            ("prompt clear", .prompt, ["clear"]), ("help seek", .help, ["seek"]),
            ("history", .history, []),
            ("search beatles", .search, ["beatles"]), ("search-next", .searchNext, []),
            ("search-previous", .searchPrevious, []),
        ]
        for (text, id, arguments) in cases {
            let result = try CommandLanguage.parse(text)
            #expect(result.id == id)
            #expect(result.arguments == arguments)
            // Every valid invocation must reject an extra argument, including optional schemas.
            #expect(throws: CommandLanguageError.self) { try CommandLanguage.parse(text + " unexpected") }
            // `search` is valid without its argument, as `help` is; its term is tested apart.
            if !arguments.isEmpty && id != .search {
                #expect(throws: CommandLanguageError.self) { try CommandLanguage.parse(id.rawValue + (id == .help ? " unknown-command" : "")) }
            }
        }
        #expect(Set(cases.map { $0.1 }) == Set(CommandID.allCases))
    }

    @Test func searchTakesAnOptionalTextTerm() throws {
        #expect(try CommandLanguage.parse("search").arguments.isEmpty)
        #expect(try CommandLanguage.parse(#"search "some term""#).arguments == ["some term"])
        let definition = try #require(CommandLanguage.registry.first { $0.id == .search })
        #expect(definition.arguments.count == 1)
        #expect(definition.arguments[0].optional)
        #expect(definition.arguments[0].kind == .text(allowEmpty: false))
        #expect(CommandLanguage.registry.first { $0.id == .searchNext } != nil)
        #expect(CommandLanguage.registry.first { $0.id == .searchPrevious } != nil)
    }

    @Test func optionalArgumentsAreNotInvented() throws {
        #expect(try CommandLanguage.parse("help").arguments.isEmpty)
        #expect(try CommandLanguage.parse("sort title").arguments == ["title"])
        #expect(try CommandLanguage.parse("sort none").arguments == ["none"])
        #expect(throws: CommandLanguageError.self) { try CommandLanguage.parse("sort none asc") }
    }

    @Test func quotesEscapesAndColonRoundTripLiteralText() throws {
        #expect(try CommandLanguage.parse(" \t: playlist-open \"Björk's Mix\" \n").arguments == ["Björk's Mix"])
        #expect(try CommandLanguage.parse(#"playlist-rename Road\ Trip "A \\ B \"C\"""#).arguments == ["Road Trip", #"A \ B "C""#])
        #expect(try CommandLanguage.parse(#"filter ab"cd ef"gh"#).arguments == ["abcd efgh"])
        #expect(try CommandLanguage.parse(#"filter '$(play); ${HOME} | next'"#).arguments == ["$(play); ${HOME} | next"])
        #expect(try CommandLanguage.parse("filter \"\"").arguments == [""])
        #expect(try CommandLanguage.parse(#"playlist-create "back\\slash""#).arguments == [#"back\slash"#])
    }

    @Test(arguments: [
        "", " : ", "PLAY", "Play", "::play", "play;pause", "play && pause", "play | next",
        "playlist-open \"unfinished", "filter 'unfinished", "filter trailing\\", "playlist-open ''",
        "playlist-create ' '", "playlist-rename Name ''", "smart-create Name ''", "filter",
        "go nowhere", "repeat true", "shuffle yes", "gain true", "resume 1", "server launch",
        "settings audio", "browser true", "drawer hidden", "artwork expanded", "group artist",
        "sort imaginary", "sort title ascending", "column title off", "column imaginary on",
        "column artist maybe", "album-sort title", "up-next-policy clear", "prompt replace",
        "rescan -1", "rescan +1", "rescan 1.0", "rescan 18446744073709551616", "rescan ١",
        "selection-move -1", "selection-move all", "browser-album end", "browser-genre ALL",
        "cover-size 95.9", "cover-size 200.1", "cover-size +100", "cover-size nan",
        "seek nan", "seek inf", "seek -inf", "seek 1e3", "seek 1ms", "seek 1.2.3", "seek +",
        "seek .", "seek 1_000", "seek ' 1'", "seek 1S", "seek １", "volume 1s", "volume --1",
        "volume +nan", "volume infinity", "volume %", "help nonexistent",
        "colour accent", "colour accent red", "colour accent #12345", "colour accent #1234567",
        "colour accent #GGGGGG", "colour background #000000", "colour Accent system", "colours",
        "colours clear", "tint toggle", "tint yes", "settings appearances",
        "lyrics sideways", "lyrics", "equalizer now",
    ])
    func rejectsInvalidInput(text: String) {
        #expect(throws: CommandLanguageError.self) { try CommandLanguage.parse(text) }
    }

    @Test func rejectsNumericOverflowWhileParsing() {
        #expect(throws: CommandLanguageError.self) { try CommandLanguage.parse("volume " + String(repeating: "9", count: 400)) }
        #expect(throws: CommandLanguageError.self) { try CommandLanguage.parse("seek " + String(repeating: "9", count: 400)) }
        #expect(throws: CommandLanguageError.self) { try CommandLanguage.parse("seek 0." + String(repeating: "0", count: 400) + "1") }
        #expect(throws: CommandLanguageError.self) { try CommandLanguage.parse("volume 0." + String(repeating: "1", count: 39)) }
    }

    @Test func settingsAppearanceParses() throws {
        #expect(try CommandLanguage.parse("settings appearance") == ParsedCommand(id: .settings, arguments: ["appearance"]))
    }

    @Test func colourCompletions() {
        #expect(CommandLanguage.completions("colour ac") == ["colour accent"])
        #expect(CommandLanguage.completions("colour accent ") == ["colour accent system"])
        #expect(CommandLanguage.completions("tint ") == ["tint off", "tint on"])
        #expect(CommandLanguage.completions("colours ") == ["colours reset"])
    }

    @Test func completionsAreContextualAndPreserveColon() throws {
        #expect(CommandLanguage.completions(":rep") == [":repeat"])
        #expect(CommandLanguage.completions("repeat o") == ["repeat off", "repeat one"])
        #expect(CommandLanguage.completions("sort title ") == ["sort title asc", "sort title desc"])
        #expect(CommandLanguage.completions("sort none ").isEmpty)
        #expect(CommandLanguage.completions("sort invalid ").isEmpty)
        #expect(CommandLanguage.completions("help sel").contains("help selection-play"))
        #expect(CommandLanguage.completions("browser-album ") == ["browser-album all"])
        #expect(CommandLanguage.completions("column album-") == ["column album-artist"])
        #expect(CommandLanguage.completions("column artist ") == ["column artist off", "column artist on", "column artist toggle"])
        #expect(CommandLanguage.completions("up-next-policy r") == ["up-next-policy replace"])
        #expect(CommandLanguage.completions("play ").isEmpty)
        #expect(CommandLanguage.completions("repeat off ").isEmpty)
        #expect(CommandLanguage.completions("playlist-open Jazz ", playlistNames: ["Jazz"]).isEmpty)
        #expect(CommandLanguage.completions("filter ").isEmpty)
    }

    @Test func goRadioCompletesAndParses() throws {
        #expect(CommandLanguage.completions("go ra") == ["go radio"])
        let command = try CommandLanguage.parse("go radio")
        #expect(command.id == .go)
        #expect(command.arguments == ["radio"])
    }

    @Test func goStatsCompletesAndParses() throws {
        #expect(CommandLanguage.completions("go st") == ["go stats"])
        let command = try CommandLanguage.parse("go stats")
        #expect(command.id == .go)
        #expect(command.arguments == ["stats"])
    }

    @Test func incompleteQuotedPlaylistCompletionProducesParseableExactNames() throws {
        let names = ["Road Trip", "Road \"Live\"", "Road \\ Home", "Road's End", "Road Trip", "Other", ""]
        for input in [":playlist-open \"Road", ":playlist-open 'Road", ":playlist-open Road\\", ":playlist-open Road"] {
            let completions = CommandLanguage.completions(input, playlistNames: names)
            #expect(completions.count == 4)
            let parsed = try completions.map { try CommandLanguage.parse($0) }
            #expect(Set(parsed.map { $0.arguments[0] }) == Set(names.filter { $0.hasPrefix("Road") }))
            #expect(parsed.allSatisfy { $0.id == .playlistOpen })
            #expect(completions.allSatisfy { $0.hasPrefix(":playlist-open ") })
        }
        let escapedSpace = CommandLanguage.completions("playlist-open Road\\ ", playlistNames: names)
        #expect(escapedSpace.count == 3)
        #expect(try escapedSpace.map { try CommandLanguage.parse($0).arguments[0] }.allSatisfy { $0.hasPrefix("Road ") })
        let unicode = CommandLanguage.completions("playlist-open 'Bjö", playlistNames: ["Björk's Mix"])
        #expect(try unicode.map { try CommandLanguage.parse($0).arguments } == [["Björk's Mix"]])
        #expect(CommandLanguage.completions("playlist-rename 'Road Trip' ", playlistNames: names).isEmpty)
    }

    /// A mark joins the grapheme before it, which may be the space or quote in front of a name.
    @Test func completionsKeepNamesWithMarksAtTheirEdges() throws {
        let names = ["\u{301}9", "\u{301}a b", "\u{1F3FB}", "a b\u{600}", "\"\u{301}", "\\\u{301}", "'\u{301} x", "x\u{200D}"]
        for name in names {
            let line = try #require(CommandLanguage.completions("playlist-delete ", playlistNames: [name, "9"]).first { $0 != "playlist-delete 9" })
            let arguments = try CommandLanguage.parse(line).arguments
            #expect(arguments.count == 1, "\(line)")
            #expect(arguments.first.map { Array($0.unicodeScalars) } == Array(name.unicodeScalars), "\(line)")
        }
    }

    /// Text with whole graphemes reads as it always did.
    @Test func wholeGraphemesReadAsBefore() throws {
        #expect(try CommandLanguage.parse("filter e\u{301}x").arguments == ["e\u{301}x"])
        #expect(try CommandLanguage.parse("filter 👨‍👩‍👧 ").arguments == ["👨‍👩‍👧"])
        #expect(try CommandLanguage.parse("filter \"a\r\nb\"").arguments == ["a\r\nb"])
        #expect(try CommandLanguage.parse("filter a\\\r\nb").arguments == ["a\r\nb"])
        #expect(try CommandLanguage.parse("playlist-rename a\r\nb").id == .playlistRename)
        #expect(try CommandLanguage.parse("playlist-rename a\r\nb").arguments == ["a", "b"])
        #expect(CommandLanguage.quote("e\u{301}") == "e\u{301}")
        #expect(CommandLanguage.quote("a\r\nb") == "\"a\r\nb\"")
    }

    @Test func suggestionsCarryQuotedLinesAndBareValues() {
        let rows = CommandLanguage.suggestions(":playlist-play R", playlistNames: ["Road Trip"])
        #expect(rows == [CommandSuggestion(line: ":playlist-play \"Road Trip\"", value: "Road Trip", summary: nil, continues: false)])
        let names = CommandLanguage.suggestions(":se")
        #expect(names.first == CommandSuggestion(line: ":seek", value: "seek",
            summary: "Seek absolutely, or relatively with a sign; default unit is seconds.", continues: true))
        #expect(names.map(\.value).contains("settings"))
        #expect(CommandLanguage.completions(":se") == names.map(\.line))
    }

    @Test func continuesFollowsRemainingArguments() {
        #expect(CommandLanguage.suggestions("play").first?.continues == false)
        #expect(CommandLanguage.suggestions("colour ac").first?.continues == true)
        #expect(CommandLanguage.suggestions("colour accent ").first?.continues == false)
        #expect(CommandLanguage.suggestions("sort n").first { $0.value == "none" }?.continues == false)
        #expect(CommandLanguage.suggestions("help").first?.continues == true)   // optional argument
    }

    @Test func usageHintOnlyAtArgumentStage() {
        #expect(CommandLanguage.usageHint(":seek +1")?.id == .seek)
        #expect(CommandLanguage.usageHint(":seek ")?.id == .seek)
        #expect(CommandLanguage.usageHint(":seek") == nil)
        #expect(CommandLanguage.usageHint(":nope 1") == nil)
    }

    @Test func historyIsARegisteredCommand() throws {
        #expect(try CommandLanguage.parse(":history") == ParsedCommand(id: .history, arguments: []))
        #expect(CommandLanguage.registry.first { $0.id == .history }?.summary == "Pick a previous command.")
    }

    /// A mark after the leading space or colon joins that grapheme. The line is read by scalar.
    @Test func leadingSpaceAndColonAreReadByScalar() throws {
        #expect((try? CommandLanguage.parse(" \u{301}next")) == nil)
        #expect((try? CommandLanguage.parse("\u{2003}\u{FE0F}: next")) == nil)
        #expect(try CommandLanguage.parse(" :next") == ParsedCommand(id: .next, arguments: []))
        #expect(CommandLanguage.suggestions(" \u{903}queue-c").isEmpty)
        #expect(CommandLanguage.suggestions(" \u{301}:queue-c").isEmpty)
        #expect(CommandLanguage.suggestions(" :queue-c").map(\.line) == [":queue-clear"])
    }

    /// The server keeps playlist names apart by their lowercase bytes, so "\u{C5}" and "A\u{30A}"
    /// can both exist. Names that are only canonically equal each get a row.
    @Test func playlistCompletionsKeepNamesThatDifferInBytes() {
        let rows = CommandLanguage.suggestions("playlist-open ", playlistNames: ["\u{C5}", "A\u{30A}", "A\u{30A}"])
        #expect(rows.map { $0.value.unicodeScalars.map(\.value) }.sorted { $0.lexicographicallyPrecedes($1) } == [[0x41, 0x30A], [0xC5]])
    }
}
