import Foundation
import Testing
@testable import TyrannyCore

@Suite struct TaggingTests {
    static let releaseID = "11111111-1111-4111-8111-111111111111"

    static func track(_ id: UInt64, matched: Bool = true, writable: Bool = true, reason: String? = nil,
                      removes: Bool = false, unwritable: [TagField] = [], changes: Int = 1) -> TrackProposal {
        TrackProposal(track_id: id, file_name: "\(id).flac", track_number: UInt32(truncatingIfNeeded: id), matched: matched, writable: writable,
                      reason: reason, removes_other_copies: removes, unwritable_fields: unwritable,
                      changes: (0..<changes).map { _ in FieldChange(field: .title, old: "a", new: "b") })
    }

    static func proposal(_ tracks: [TrackProposal]) -> TagProposal {
        TagProposal(proposal_id: "p1", release_id: releaseID, candidates: [], tracks: tracks)
    }

    @Test func decodesTheServerShapes() throws {
        let decoder = JSONDecoder()
        let proposal = try decoder.decode(TagProposalResponse.self, from: Data("""
        {"kind":"tag_proposal","proposal":{"proposal_id":"p1","release_id":"\(Self.releaseID)",
         "candidates":[{"release_id":"\(Self.releaseID)","title":"Fixture Album","artist":"Fixture Artist","date":"2001-02-03","country":"GB","track_count":3,"score":100}],
         "tracks":[{"track_id":18446744073709551615,"file_name":"01.flac","track_number":1,"matched":true,"writable":true,"reason":null,
                    "removes_other_copies":false,"unwritable_fields":["disc_total"],
                    "changes":[{"field":"musicbrainz_release_id","old":null,"new":"x"}]}]}}
        """.utf8)).proposal
        #expect(proposal.tracks[0].track_id == .max)
        #expect(proposal.tracks[0].unwritable_fields == [.discTotal])
        #expect(proposal.tracks[0].changes[0].field == .musicbrainzReleaseID)
        #expect(proposal.candidates[0].score == 100)

        let apply = try decoder.decode(TagApplyResult.self, from: Data("""
        {"kind":"tag_apply","edit_id":3,"files":[{"track_id":1,"file_name":"a.flac","outcome":"written","message":null},
        {"track_id":2,"file_name":"b.flac","outcome":"skipped","message":"changed since the lookup"}]}
        """.utf8))
        #expect(apply.edit_id == 3)
        #expect(apply.files.map(\.outcome) == [.written, .skipped])
        let none = try decoder.decode(TagApplyResult.self, from: Data(#"{"kind":"tag_apply","edit_id":null,"files":[]}"#.utf8))
        #expect(none.edit_id == nil)

        let edits = try decoder.decode(TagEditsResponse.self, from: Data("""
        {"kind":"tag_edits","edits":[{"edit_id":3,"created_at":1792575000,"release_title":"Fixture Album","file_count":3,"reverted":false}]}
        """.utf8))
        #expect(edits.edits[0].edit_id == 3)
        #expect(try decoder.decode(TagEditsResponse.self, from: Data(#"{"kind":"tag_edits","edits":[]}"#.utf8)).edits.isEmpty)
    }

    @Test func fieldLabelsCoverEveryField() {
        #expect(TagField.allCases.count == 16)
        #expect(TagField.allCases.allSatisfy { !$0.label.isEmpty })
        #expect(TagField.musicbrainzReleaseGroupID.label == "MusicBrainz Release Group ID")
    }

    @Test func candidateLabels() {
        func candidate(date: String?, country: String?, count: UInt32) -> TagCandidate {
            TagCandidate(release_id: "r", title: "Fixture Album", artist: "Fixture Artist", date: date, country: country, track_count: count, score: 1)
        }
        #expect(candidate(date: "2001-02-03", country: "GB", count: 3).label == "Fixture Album — Fixture Artist · 2001 · GB · 3 tracks")
        #expect(candidate(date: nil, country: nil, count: 4).label == "Fixture Album — Fixture Artist · 4 tracks")
        #expect(candidate(date: "1999", country: nil, count: 1).label == "Fixture Album — Fixture Artist · 1999 · 1 track")
    }

    @Test func changeLinesAndNotes() {
        #expect(FieldChange(field: .album, old: "Old Album", new: "Fixture Album").line == "Album: Old Album → Fixture Album")
        #expect(FieldChange(field: .musicbrainzReleaseID, old: nil, new: Self.releaseID).line
                == "MusicBrainz Release ID: (none) → \(Self.releaseID)")
        #expect(Self.track(1, matched: false).notes == ["No matching track on this release"])
        #expect(Self.track(1, writable: false, reason: "Tyranny can't write tags to this format").notes
                == ["Tyranny can't write tags to this format"])
        #expect(Self.track(1, changes: 0).notes == ["Already matches"])
        #expect(Self.track(1).notes == [])
        #expect(Self.track(1, removes: true, unwritable: [.discTotal, .originalDate]).notes
                == ["can't write Disc Total to this format", "can't write Original Date to this format",
                    "Also removes old copies of these fields from other tags"])
    }

    @Test func selectionDefaultsAndToggling() {
        let proposal = Self.proposal([
            Self.track(1), Self.track(2), Self.track(3), Self.track(4, matched: false),
            Self.track(5, writable: false), Self.track(6, changes: 0),
        ])
        var selection = TagSelection(proposal)
        #expect(selection.selected == [1, 2, 3])
        #expect(selection.writeButtonTitle == "Write Tags to 3 Files")
        selection.toggle(4); selection.toggle(5); selection.toggle(6); selection.toggle(99)
        #expect(selection.selected == [1, 2, 3])
        selection.toggle(2); selection.toggle(3)
        #expect(selection.writeButtonTitle == "Write Tags to 1 File")
        selection.toggle(1)
        #expect(selection.applyRequest() == nil)
        selection.toggle(3)
        #expect(selection.selected == [3])
    }

    @Test func applyRequestUsesProposalOrderAndNoValues() {
        var selection = TagSelection(Self.proposal([Self.track(9), Self.track(2), Self.track(5)]))
        selection.toggle(2)
        selection.toggle(2)
        let request = selection.applyRequest()
        #expect(request?.name == "apply_tags")
        #expect(request?.fields == ["proposal_id": .string("p1"), "track_ids": .uints([9, 2, 5])])
    }

    @Test func applyRequestForFiveHundredTracksFitsOneFrame() throws {
        let tracks = (0..<500).map { Self.track(.max - UInt64($0)) }
        let request = try #require(TagSelection(Self.proposal(tracks)).applyRequest())
        let frame = try RequestFrame.encode(request.name, fields: request.fields, id: .max)
        #expect(frame.count <= RequestFrame.limit)
    }

    @Test func lookupRequestFields() {
        let plain = TagRequests.lookup(albumID: 7, libraryRevision: 9, releaseID: nil, acoustIDKey: nil)
        #expect(plain.name == "musicbrainz_lookup")
        #expect(plain.fields == ["album_id": .uint(7), "library_revision": .uint(9)])
        let full = TagRequests.lookup(albumID: 7, libraryRevision: 9, releaseID: "r1", acoustIDKey: "KEY123")
        #expect(full.fields == ["album_id": .uint(7), "library_revision": .uint(9), "release_id": .string("r1"),
                                "fingerprint": .bool(true), "acoustid_key": .string("KEY123")])
        #expect(TagRequests.edits.name == "tag_edits")
        #expect(TagRequests.revert(editID: 4).fields == ["edit_id": .uint(4)])
    }

    @Test func editRowText() throws {
        let utc = try #require(TimeZone(identifier: "UTC"))
        var edit = TagEditSummary(edit_id: 1, created_at: 1792575000, release_title: "Fixture Album", file_count: 3, reverted: false)
        #expect(edit.rowText(timeZone: utc) == "2026-10-21 09:30 · Fixture Album · 3 files")
        edit = TagEditSummary(edit_id: 1, created_at: 1792575000, release_title: "Fixture Album", file_count: 1, reverted: true)
        #expect(edit.rowText(timeZone: utc) == "2026-10-21 09:30 · Fixture Album · 1 file · reverted")
    }

    @Test func failureMessages() {
        func text(_ code: String, _ message: String) -> String { TagLookupMessage.text(for: ServerFailure(code: code, message: message)) }
        #expect(text("lookup", "MusicBrainz is busy, try again shortly") == "MusicBrainz is busy, try again shortly")
        #expect(text("invalid_command", "lookup expired, look the album up again") == "This lookup expired. Look the album up again.")
        #expect(text("invalid_command", "bad field") == "bad field")
        #expect(text("stale_library_revision", "x") == "The library changed. Look the album up again.")
        #expect(text("request_timeout", "x") == "MusicBrainz took too long to answer.")
        #expect(text("other", "plain") == "plain")
    }

    @Test func editFailureMessages() {
        func text(_ code: String, _ message: String) -> String { TagEditMessage.text(for: ServerFailure(code: code, message: message)) }
        #expect(text("request_timeout", "x") == "Still writing tags. Check Recent Tag Edits in Settings.")
        #expect(text("invalid_command", "lookup expired, look the album up again") == "This lookup expired. Look the album up again.")
        #expect(text("other", "plain") == "plain")
    }

    @Test func editListTimeoutMessage() {
        func text(_ code: String, _ message: String) -> String { TagEditMessage.listText(for: ServerFailure(code: code, message: message)) }
        #expect(text("request_timeout", "x") == "Recent tag edits took too long to load. Try again.")
        #expect(text("lookup", "MusicBrainz is busy, try again shortly") == "MusicBrainz is busy, try again shortly")
    }

    @Test func requestDeadlines() {
        // Writes are never cancelled, so the edit lane waits far longer than a lookup.
        #expect(TagRequests.lookupDeadlineSeconds == 60)
        #expect(TagRequests.editDeadlineSeconds == 30 * 60)
    }

    @Test func acoustIDKeyNormalization() {
        #expect(AcoustIDKey.normalized("  ab12CD  ") == "ab12CD")
        #expect(AcoustIDKey.normalized("") == nil)
        #expect(AcoustIDKey.normalized("has space") == nil)
        #expect(AcoustIDKey.normalized("ü") == nil)
        #expect(AcoustIDKey.normalized(String(repeating: "a", count: 65)) == nil)
        #expect(AcoustIDKey.normalized(String(repeating: "a", count: 64)) == String(repeating: "a", count: 64))
    }
}
