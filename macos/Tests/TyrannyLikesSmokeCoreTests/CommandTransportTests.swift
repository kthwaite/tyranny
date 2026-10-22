import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct CommandTransportTests {
    @Test func seekUsesUnitsAndAuthoritativeFramePosition() throws {
        let state = snapshot(position: 480_017, duration: 20_000_000)
        #expect(try request("seek 1.5", state).fields == ["entry_id": .uint(7), "position_frames": .uint(72_000)])
        #expect(try request("seek +1.5s", state).fields["position_frames"] == .uint(552_017))
        #expect(try request("seek -0.5s", state).fields["position_frames"] == .uint(456_017))
        #expect(try request("seek 1m", state).fields["position_frames"] == .uint(2_880_000))
        #expect(try request("seek .01h", state).fields["position_frames"] == .uint(1_728_000))
        #expect(try request("seek 25%", state).fields["position_frames"] == .uint(5_000_000))
        #expect(try request("seek +1%", state).fields["position_frames"] == .uint(680_017))
        #expect(try request("seek -1%", state).fields["position_frames"] == .uint(280_017))
    }

    @Test func seeksClampToLastPlayableFrameAndNeverUnderflow() throws {
        let state = snapshot(position: 480_017, duration: 960_000)
        #expect(try request("seek 100%", state).fields["position_frames"] == .uint(959_999))
        #expect(try request("seek 999s", state).fields["position_frames"] == .uint(959_999))
        #expect(try request("seek +100s", state).fields["position_frames"] == .uint(959_999))
        #expect(try request("seek -999s", state).fields["position_frames"] == .uint(0))
        #expect(try request("seek 0", state).fields["position_frames"] == .uint(0))
        #expect(try request("seek 100%", snapshot(duration: 1)).fields["position_frames"] == .uint(0))
        #expect(try request("seek -0", state).fields["position_frames"] == .uint(480_017))
        #expect(try request("seek +0", state).fields["position_frames"] == .uint(480_017))
    }

    @Test func relativeFramesRemainExactBeyondDoubleIntegerPrecision() throws {
        let state = snapshot(position: 9_007_199_254_740_993, duration: UInt64.max, rate: 1)
        #expect(try request("seek +1", state).fields["position_frames"] == .uint(9_007_199_254_740_994))
        #expect(try request("seek -1", state).fields["position_frames"] == .uint(9_007_199_254_740_992))
        #expect(try request("seek +0.5", state).fields["position_frames"] == .uint(9_007_199_254_740_993))
        #expect(try request("seek -0.5", state).fields["position_frames"] == .uint(9_007_199_254_740_992))
        #expect(try request("seek 100%", state).fields["position_frames"] == .uint(UInt64.max - 1))
        #expect(try request("seek 18446744073709551615s", state).fields["position_frames"] == .uint(UInt64.max - 1))
    }

    @Test func seekRejectsMissingOrStaleTrackAndInvalidTiming() {
        for state in [
            snapshot(current: nil), snapshot(nowPlayingID: nil), snapshot(nowPlayingID: 8),
            snapshot(duration: nil), snapshot(duration: 0), snapshot(rate: 0),
        ] {
            for text in ["seek 1s", "seek +1s", "seek 10%"] {
                #expect(throws: CommandLanguageError.self) { try request(text, state) }
            }
        }
    }

    @Test func overflowingSeeksAreRejectedRatherThanTrappingOrClamping() {
        #expect(throws: CommandLanguageError.self) { try request("seek 18446744073709551616s", snapshot(rate: 1)) }
        #expect(throws: CommandLanguageError.self) { try request("seek 18446744073709551615h", snapshot()) }
        #expect(throws: CommandLanguageError.self) { try request("seek +1s", snapshot(position: UInt64.max, duration: UInt64.max, rate: 1)) }
        #expect(throws: CommandLanguageError.self) { try request("seek 101%", snapshot(duration: UInt64.max)) }
        #expect(throws: CommandLanguageError.self) { try request("seek -18446744073709551616s", snapshot(rate: 1)) }
        #expect(throws: CommandLanguageError.self) { try request("seek nan", snapshot()) }
    }

    @Test func volumeUsesSnapshotPercentageAndRoundsAfterAdjustment() throws {
        let state = snapshot(volume: 63)
        #expect(try request("volume 20", state) == CommandRequest("set_volume", ["percent": .int(20)]))
        #expect(try request("volume +5%", state).fields["percent"] == .int(68))
        #expect(try request("volume -5", state).fields["percent"] == .int(58))
        #expect(try request("volume 19.5", state).fields["percent"] == .int(20))
        #expect(try request("volume -0.5", state).fields["percent"] == .int(63))
        #expect(try request("volume 0", state).fields["percent"] == .int(0))
        #expect(try request("volume -0", state).fields["percent"] == .int(63))
        #expect(try request("volume +1000", state).fields["percent"] == .int(100))
        #expect(try request("volume -1000", state).fields["percent"] == .int(0))
        #expect(try request("volume 150", state).fields["percent"] == .int(100))
    }

    @Test func modesResolveAgainstTheProvidedSnapshot() throws {
        #expect(try request("repeat cycle", snapshot(repeatMode: .off)).fields == ["mode": .string("all")])
        #expect(try request("repeat cycle", snapshot(repeatMode: .all)).fields == ["mode": .string("one")])
        #expect(try request("repeat cycle", snapshot(repeatMode: .one)).fields == ["mode": .string("off")])
        #expect(try request("repeat off", snapshot(repeatMode: .one)) == CommandRequest("set_repeat", ["mode": .string("off")]))
        #expect(try request("shuffle toggle", snapshot(shuffle: false)) == CommandRequest("set_shuffle", ["enabled": .bool(true)]))
        #expect(try request("shuffle toggle", snapshot(shuffle: true)).fields["enabled"] == .bool(false))
        #expect(try request("shuffle on", snapshot(shuffle: true)).fields["enabled"] == .bool(true))
        #expect(try request("shuffle off", snapshot(shuffle: true)).fields["enabled"] == .bool(false))
        #expect(try request("resume toggle", snapshot(resume: false)) == CommandRequest("set_resume", ["enabled": .bool(true)]))
        #expect(try request("resume toggle", snapshot(resume: true)).fields["enabled"] == .bool(false))
        #expect(try request("resume off", snapshot(resume: true)).fields["enabled"] == .bool(false))
        #expect(try request("gain track", snapshot()) == CommandRequest("set_gain", ["mode": .string("track")]))
    }

    @Test func transportAvailabilityUsesServerCapabilities() throws {
        let state = snapshot()
        #expect(try request("play", state) == CommandRequest("play", ["entry_id": .null]))
        #expect(try request("toggle", state) == CommandRequest("toggle_play_pause"))
        #expect(try request("pause", state) == CommandRequest("pause"))
        #expect(try request("stop", state) == CommandRequest("stop"))
        #expect(try request("next", state) == CommandRequest("next"))
        #expect(try request("previous", state) == CommandRequest("previous"))
        #expect(throws: CommandLanguageError.self) { try request("play", snapshot(queueLength: 0)) }
        #expect(throws: CommandLanguageError.self) { try request("toggle", snapshot(queueLength: 0)) }
        #expect(throws: CommandLanguageError.self) { try request("pause", snapshot(current: nil)) }
        #expect(throws: CommandLanguageError.self) { try request("next", snapshot(canNext: false)) }
        #expect(throws: CommandLanguageError.self) { try request("previous", snapshot(canPrevious: false)) }
        // Stopping remains safe and idempotent with no current entry.
        #expect(try request("stop", snapshot(current: nil, nowPlayingID: nil, queueLength: 0)).name == "stop")
    }

    @Test func localActionsNeverBecomeArbitraryWireRequests() throws {
        for text in ["go albums", "playlist-play Jazz", "selection-next", "server stop", "queue-clear", "help play"] {
            #expect(try CommandLanguage.transportRequest(CommandLanguage.parse(text), state: snapshot()) == nil)
        }
        for text in ["set_volume 10", "set_repeat all", "seek 1 entry_id 9"] {
            #expect(throws: CommandLanguageError.self) { try CommandLanguage.parse(text) }
        }
    }

    private func request(_ text: String, _ state: ControlState) throws -> CommandRequest {
        let result = try CommandLanguage.transportRequest(CommandLanguage.parse(text), state: state)
        return try #require(result)
    }

    private func snapshot(
        current: UInt64? = 7, nowPlayingID: UInt64? = 7, position: UInt64 = 480_000,
        duration: UInt64? = 4_800_000, rate: UInt32 = 48_000, queueLength: Int = 3,
        canNext: Bool = true, canPrevious: Bool = true, volume: UInt8 = 70,
        repeatMode: RepeatMode = .off, shuffle: Bool = false, resume: Bool = false
    ) -> ControlState {
        let playing = nowPlayingID.map {
            NowPlaying(entry_id: $0, title: "Track", artist: "Artist", album: "Album",
                       duration_frames: duration, metadata_error: nil, track_id: 1,
                       album_id: 2, album_artist_id: 3, compilation: false, format: "FLAC", gain_db: nil)
        }
        return ControlState(
            sequence: 8, queue_revision: 4, playlists_revision: 1, stations_revision: 0, queue_length: queueLength,
            current_id: current, can_next: canNext, can_previous: canPrevious, now_playing: playing,
            position_frames: position, sample_rate: rate, transport: "playing",
            settings: PlayerSettings(gain_mode: "off", resume_on_start: resume, volume: volume, repeat: repeatMode, shuffle: shuffle, equalizer: EqualizerSettings()),
            error: nil, underruns: 0,
            library: LibraryState(revision: 1, roots: 0, tracks: 0, duration_ms: 0, size_bytes: 0,
                                  scan: ScanStatus(root_id: nil, phase: "idle", discovered: 0, processed: 0,
                                                   total: nil, updated: 0, unchanged: 0, failures: 0, skipped: 0, last_error: nil)),
            history_revision: 0, stats_revision: 0, current_index: current == nil ? nil : 1, upcoming_count: 1)
    }
}
