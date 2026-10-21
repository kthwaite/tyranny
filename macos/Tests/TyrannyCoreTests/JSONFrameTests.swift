import Foundation
import Testing
@testable import TyrannyCore

@Suite struct JSONFrameTests {
    @Test func lengthCountsUTF8BytesAndUsesBigEndian() throws {
        let payload = Data("{\"title\":\"音楽\"}".utf8)
        let frame = try JSONFrame.encode(payload, limit: 16 * 1024)
        #expect(Array(frame.prefix(4)) == [0, 0, 0, UInt8(payload.count)])
        #expect(try JSONFrame.payloadLength(Data(frame.prefix(4)), limit: 8 * 1024 * 1024) == payload.count)
        #expect(Data(frame.dropFirst(4)) == payload)
    }

    @Test func rejectsIncompleteAndOverlongHeaders() {
        for count in [0, 1, 2, 3, 5] {
            #expect(throws: JSONFrame.Failure.invalidHeader) {
                try JSONFrame.payloadLength(Data(repeating: 1, count: count), limit: 8 * 1024 * 1024)
            }
        }
    }

    @Test func validatesResponseLengthBeforeReadingPayload() throws {
        let limit = 8 * 1024 * 1024
        #expect(try JSONFrame.payloadLength(Data([0, 0, 0, 1]), limit: limit) == 1)
        #expect(try JSONFrame.payloadLength(Data([0, 0x80, 0, 0]), limit: limit) == limit)
        let headers: [[UInt8]] = [[0, 0, 0, 0], [0, 0x80, 0, 1], [0xff, 0xff, 0xff, 0xff]]
        for header in headers {
            #expect(throws: JSONFrame.Failure.invalidLength) {
                try JSONFrame.payloadLength(Data(header), limit: limit)
            }
        }
    }

    @Test func requestLimitIsInclusiveAndEmptyFramesAreRejected() throws {
        let limit = 16 * 1024
        let frame = try JSONFrame.encode(Data(repeating: 0x20, count: limit), limit: limit)
        #expect(Array(frame.prefix(4)) == [0, 0, 0x40, 0])
        #expect(frame.count == limit + 4)
        for count in [0, limit + 1] {
            #expect(throws: JSONFrame.Failure.invalidLength) {
                try JSONFrame.encode(Data(repeating: 0x20, count: count), limit: limit)
            }
        }
    }
}
