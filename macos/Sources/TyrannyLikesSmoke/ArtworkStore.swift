import CoreGraphics
import Foundation
import ImageIO
import Observation
import UniformTypeIdentifiers
import TyrannyLikesSmokeCore

enum ArtworkSize: String, Hashable, Sendable {
    case thumbnail, display
    var maximumDimension: Int { self == .thumbnail ? 128 : 512 }
}

/// A display's identity survives catalog edits; each fetch is pinned to the current revision.
struct ArtworkRequest: Hashable, Sendable {
    enum Target: Hashable, Sendable { case album(UInt64), entry(UInt64) }
    let target: Target
    let generation: UInt64
    let size: ArtworkSize

    func fields(revision: UInt64) -> [String: JSONValue] {
        var fields: [String: JSONValue] = [
            "album_id": .null, "entry_id": .null,
            "library_revision": .uint(revision), "size": .string(size.rawValue),
        ]
        switch target {
        case .album(let id): fields["album_id"] = .uint(id)
        case .entry(let id): fields["entry_id"] = .uint(id)
        }
        return fields
    }
}

/// Decoding finishes before this handle reaches the main actor. The store revokes restricted
/// pixels on release/expiry so recycled native cells cannot retain a reusable image copy.
private final class ArtworkPixels: @unchecked Sendable {
    var image: CGImage?
    init(_ image: CGImage?) { self.image = image }
}

struct ArtworkImage: @unchecked Sendable {
    private let pixels: ArtworkPixels
    var image: CGImage? { pixels.image }
    let state: String
    let cacheKey: String?
    var sourceURL: URL? = nil
    var expiresAt: Date? = nil
    var cachePolicy: ArtworkCachePolicy = .persistent
    var credentialFailure = false
    var requiresAttribution: Bool { state == "deezer" || state == "discogs" }
    var byteCost: Int { image.map { $0.bytesPerRow * $0.height } ?? 0 }
    static let loading = ArtworkImage(image: nil, state: "loading", cacheKey: nil)
    static let missing = ArtworkImage(image: nil, state: "missing", cacheKey: nil)
    static let unavailable = ArtworkImage(image: nil, state: "unavailable", cacheKey: nil)

    init(image: CGImage?, state: String, cacheKey: String?, sourceURL: URL? = nil,
         expiresAt: Date? = nil, cachePolicy: ArtworkCachePolicy = .persistent,
         credentialFailure: Bool = false) {
        pixels = ArtworkPixels(image)
        self.state = state
        self.cacheKey = cacheKey
        self.sourceURL = sourceURL
        self.expiresAt = expiresAt
        self.cachePolicy = cachePolicy
        self.credentialFailure = credentialFailure
    }

    @MainActor func revokeRestrictedPixels() {
        if requiresAttribution { pixels.image = nil }
    }
}

/// Two reusable connections: browsing cannot queue ahead of Now Playing or main controls.
/// Only visible albums and the authoritative current occurrence create demand.
@MainActor @Observable
final class ArtworkStore {
    private struct Context: Equatable {
        let socketPath: String
        let instance: String
        let revision: UInt64
    }
    private struct Cached {
        let value: ArtworkImage
        let revision: UInt64
        let expires: Date
    }
    @MainActor private final class Lane {
        // Discovery and provider deadlines are additive; main controls retain 10s.
        let wire = WireClient(requestDeadlineSeconds: 30)
        var task: Task<Void, Never>?
        var request: ArtworkRequest?
        var ticket = UUID()
    }

    let settings = ArtworkSettings()
    @ObservationIgnored private var expiryTimer: Timer?

    init(diagnostics: DiagnosticsLog? = nil) {
        thumbnails.wire.diagnostics = diagnostics
        display.wire.diagnostics = diagnostics
        settings.onChange = { [weak self] in
            guard let self else { return }
            let entryID: UInt64?
            if case .entry(let id) = nowPlayingRequest?.target { entryID = id } else { entryID = nil }
            synchronize(socketPath: context?.socketPath ?? "",
                        instance: context?.instance, revision: context?.revision,
                        entryID: entryID, invalidate: true)
        }
    }
    private(set) var generation: UInt64 = 0
    private(set) var nowPlayingRequest: ArtworkRequest?
    /// Called after the now-playing request or its image may have changed.
    @ObservationIgnored var onNowPlayingChange: (@MainActor () -> Void)?
    private var cache: [ArtworkRequest: Cached] = [:]
    @ObservationIgnored private var context: Context?
    @ObservationIgnored private var demand: [ArtworkRequest: Int] = [:]
    @ObservationIgnored private var pending: [ArtworkRequest] = []
    @ObservationIgnored private var recency: [ArtworkRequest] = []
    @ObservationIgnored private var cacheBytes = 0
    @ObservationIgnored private let thumbnails = Lane()
    @ObservationIgnored private let display = Lane()
    /// The album grid at its smallest covers shows well over 128 covers on a large display.
    private static let maximumEntries = 256
    private static let maximumBytes = 24 * 1024 * 1024

    func synchronize(socketPath: String, instance: String?, revision: UInt64?, entryID: UInt64?, invalidate: Bool = false) {
        let next: Context?
        if let instance, let revision {
            next = Context(socketPath: socketPath, instance: instance, revision: revision)
        } else {
            next = nil
        }
        let revisionChanged = next?.revision != context?.revision
        if invalidate || next?.socketPath != context?.socketPath || next?.instance != context?.instance {
            cancel(thumbnails)
            expiryTimer?.invalidate()
            expiryTimer = nil
            cancel(display)
            context = next
            generation &+= 1
            nowPlayingRequest = nil
            demand.removeAll()
            for cached in cache.values { cached.value.revokeRestrictedPixels() }
            pending.removeAll()
            cache.removeAll()
            recency.removeAll()
            cacheBytes = 0
        } else if revisionChanged {
            // The same cells and occurrence still own their requests. Refresh them in place,
            // without giving up their pixels, attribution, or original provider expiry.
            context = next
            if thumbnails.task != nil { cancel(thumbnails) }
            if display.task != nil { cancel(display) }
            pending = Array(demand.keys)
        }
        let request = next == nil ? nil : entryID.map {
            ArtworkRequest(target: .entry($0), generation: generation, size: .display)
        }
        if request != nowPlayingRequest {
            if let previous = nowPlayingRequest { release(previous) }
            nowPlayingRequest = request
            if let request { retain(request) }
        }
        if revisionChanged {
            pump(thumbnails)
            pump(display)
        }
        onNowPlayingChange?()
    }

    /// Whether a server session exists. Reading `generation` makes context changes observable.
    var hasContext: Bool {
        _ = generation
        return context != nil
    }

    func albumRequest(_ albumID: UInt64) -> ArtworkRequest? {
        // Reading generation makes context changes observable without exposing credentials.
        let generation = generation
        guard context != nil else { return nil }
        return ArtworkRequest(target: .album(albumID), generation: generation, size: .thumbnail)
    }

    func value(for request: ArtworkRequest?) -> ArtworkImage {
        guard let request, request.generation == generation else { return .missing }
        guard let cached = cache[request],
              cached.revision == context?.revision || demand[request] != nil,
              cached.value.expiresAt.map({ $0 > Date() }) ?? true else { return .loading }
        return cached.value
    }

    var nowPlaying: ArtworkImage { value(for: nowPlayingRequest) }

    func retain(_ request: ArtworkRequest) {
        guard request.generation == generation, context != nil else { return }
        if let count = demand[request] {
            demand[request] = count + 1
            return
        }
        guard demand.count < Self.maximumEntries else { return }
        demand[request] = 1
        if let cached = cache[request], cached.revision == context?.revision, cached.expires > Date() {
            touch(request)
            return
        }
        removeCached(request)
        pending.append(request)
        pump(request.size == .thumbnail ? thumbnails : display)
    }

    func release(_ request: ArtworkRequest) {
        guard let count = demand[request] else { return }
        if count > 1 { demand[request] = count - 1; return }
        demand.removeValue(forKey: request)
        pending.removeAll { $0 == request }
        if let cached = cache[request], cached.revision != context?.revision
            || !ArtworkRetention.mayRedisplay(policy: cached.value.cachePolicy, consumers: 0) {
            removeCached(request)
        }
        let lane = request.size == .thumbnail ? thumbnails : display
        if lane.request == request {
            cancel(lane)
            pump(lane)
        }
    }

    private func cancel(_ lane: Lane) {
        lane.ticket = UUID()
        lane.task?.cancel()
        lane.task = nil
        lane.request = nil
        lane.wire.disconnect()
    }

    private func pump(_ lane: Lane) {
        guard lane.task == nil, let context,
              let index = pending.firstIndex(where: { ($0.size == .thumbnail) == (lane === thumbnails) }) else { return }
        let request = pending.remove(at: index)
        let ticket = UUID()
        lane.ticket = ticket
        lane.request = request
        let providerFields = settings.requestFields
        lane.task = Task { [weak self] in
            let value: ArtworkImage?
            do {
                if lane.wire.instanceID == nil {
                    try await lane.wire.connect(socketPath: context.socketPath)
                }
                guard lane.wire.instanceID == context.instance else {
                    throw ServerFailure(code: "artwork_session_changed", message: "Artwork belongs to a different server session.")
                }
                let fields = request.fields(revision: context.revision).merging(providerFields) { _, provider in provider }
                let data = try await lane.wire.request("artwork", fields: fields)
                try Task.checkCancellation()
                let decoding = Task.detached(priority: .utility) { ArtworkDecoder.decode(data, size: request.size) }
                value = await withTaskCancellationHandler {
                    await decoding.value
                } onCancel: {
                    decoding.cancel()
                }
            } catch let failure as ServerFailure where failure.code == "stale_library_revision" {
                // The next control snapshot schedules a fetch for the new revision.
                value = nil
            } catch {
                value = .unavailable
            }
            guard let self, !Task.isCancelled, lane.ticket == ticket,
                  self.context == context, request.generation == generation, demand[request] != nil else { return }
            if let value { insert(value, for: request, revision: context.revision) }
            lane.request = nil
            lane.task = nil
            pump(lane)
        }
    }

    private func insert(_ value: ArtworkImage, for request: ArtworkRequest, revision: UInt64) {
        let value = value.state == "deezer" && ArtworkBrand.deezerLogo == nil ? ArtworkImage.unavailable : value
        if value.credentialFailure { settings.reportCredentialFailure() }
        let lifetime: TimeInterval = value.state == "unavailable" ? 60 : 24 * 60 * 60
        let expires = value.expiresAt ?? Date().addingTimeInterval(lifetime)
        // Native cells may still hold the previous handle. Don't revoke identical pixels
        // between their redraws, or transfer them to different attribution or expiry terms.
        if let old = cache[request]?.value, let key = value.cacheKey, old.cacheKey == key,
           old.state == value.state, old.sourceURL == value.sourceURL, old.cachePolicy == value.cachePolicy,
           old.expiresAt == value.expiresAt, old.credentialFailure == value.credentialFailure {
            cache[request] = Cached(value: old, revision: revision, expires: expires)
            touch(request)
            return
        }
        removeCached(request)
        while cache.count >= Self.maximumEntries || cacheBytes + value.byteCost > Self.maximumBytes {
            guard let oldest = recency.first(where: { demand[$0] == nil }) ?? recency.first else { break }
            removeCached(oldest)
        }
        cache[request] = Cached(value: value, revision: revision, expires: expires)
        cacheBytes += value.byteCost
        touch(request)
        if request == nowPlayingRequest { onNowPlayingChange?() }
        scheduleExpiry()
    }

    private func removeCached(_ request: ArtworkRequest) {
        if let old = cache.removeValue(forKey: request) {
            cacheBytes -= old.value.byteCost
            old.value.revokeRestrictedPixels()
        }
        recency.removeAll { $0 == request }
    }

    private func touch(_ request: ArtworkRequest) {
        recency.removeAll { $0 == request }
        recency.append(request)
    }

    private func scheduleExpiry() {
        expiryTimer?.invalidate()
        expiryTimer = nil
        guard let deadline = cache.values.compactMap({ $0.value.expiresAt }).min() else { return }
        let timer = Timer(fire: deadline, interval: 0, repeats: false) { [weak self] _ in
            Task { @MainActor in self?.expireArtwork() }
        }
        expiryTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    private func expireArtwork() {
        let now = Date()
        let expired = cache.compactMap { request, cached in
            cached.value.expiresAt.map { $0 <= now } == true ? request : nil
        }
        for request in expired {
            // Remove first, including active displays: a failed refresh must never reveal stale art.
            removeCached(request)
            let lane = request.size == .thumbnail ? thumbnails : display
            if demand[request] != nil, lane.request != request, !pending.contains(request) { pending.append(request) }
        }
        if let request = nowPlayingRequest, expired.contains(request) {
            onNowPlayingChange?()
        }
        scheduleExpiry()
        pump(thumbnails)
        pump(display)
    }
}

private enum ArtworkDecoder {
    private struct Reply: Decodable {
        let kind: String
        let artwork: Payload
    }
    private struct Payload: Decodable {
        enum Status: String, Decodable { case ready, missing, unavailable }
        enum Source: String, Decodable { case embedded, folder, cover_art_archive, deezer, discogs }
        let status: Status
        let source: Source?
        let cache_key: String?
        let width: UInt32
        let height: UInt32
        let mime_type: String?
        let data: String?
        let message: String?
        let source_url: URL?
        let expires_at: Double?
        let cache_policy: ArtworkCachePolicy
    }
    private static let maximumEncodedBytes = 2 * 1024 * 1024

    static func decode(_ data: Data, size: ArtworkSize) -> ArtworkImage {
        guard !Task.isCancelled,
              let reply = try? JSONDecoder().decode(Reply.self, from: data), reply.kind == "artwork" else { return .unavailable }
        let art = reply.artwork
        guard art.status == .ready else {
            guard art.data == nil, art.width == 0, art.height == 0 else { return .unavailable }
            return ArtworkImage(image: nil, state: art.status == .missing ? "missing" : "unavailable",
                                cacheKey: nil, credentialFailure: art.message == "Discogs rejected credentials or access")
        }
        let expiry = art.expires_at.map(Date.init(timeIntervalSince1970:))
        guard let provenance = art.source,
              ArtworkRetention.valid(source: provenance.rawValue, policy: art.cache_policy,
                                     sourceURL: art.source_url, expiresAt: expiry, now: Date()) else { return .unavailable }
        guard let source = art.source, let key = art.cache_key, !key.isEmpty, key.utf8.count <= 256,
              art.mime_type == "image/jpeg", art.width > 0, art.height > 0,
              Int(art.width) <= size.maximumDimension, Int(art.height) <= size.maximumDimension,
              let encoded = art.data, encoded.utf8.count <= ((maximumEncodedBytes + 2) / 3) * 4,
              let bytes = Data(base64Encoded: encoded), bytes.count <= maximumEncodedBytes,
              !Task.isCancelled else { return .unavailable }
        let options = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let imageSource = CGImageSourceCreateWithData(bytes as CFData, options),
              CGImageSourceGetType(imageSource) as String? == UTType.jpeg.identifier,
              CGImageSourceGetCount(imageSource) == 1,
              let properties = CGImageSourceCopyPropertiesAtIndex(imageSource, 0, options) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? NSNumber,
              let height = properties[kCGImagePropertyPixelHeight] as? NSNumber,
              width.intValue == Int(art.width), height.intValue == Int(art.height),
              let depth = properties[kCGImagePropertyDepth] as? NSNumber, depth.intValue <= 8,
              !Task.isCancelled else { return .unavailable }
        // Actual headers, not just wire dimensions, are checked before decompression.
        let decodeOptions = [kCGImageSourceShouldCache: true, kCGImageSourceShouldCacheImmediately: true] as CFDictionary
        guard let image = CGImageSourceCreateImageAtIndex(imageSource, 0, decodeOptions),
              image.width == Int(art.width), image.height == Int(art.height),
              image.bitsPerPixel <= 32, !Task.isCancelled else { return .unavailable }
        return ArtworkImage(image: image, state: source.rawValue, cacheKey: key,
                            sourceURL: art.source_url, expiresAt: expiry, cachePolicy: art.cache_policy,
                            credentialFailure: art.message == "Discogs rejected credentials or access")
    }
}
