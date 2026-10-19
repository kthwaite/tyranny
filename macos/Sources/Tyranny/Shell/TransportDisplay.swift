import AppKit
import SwiftUI
import TyrannyCore

/// The centred toolbar display, as in iTunes: thumbnail, title, "artist — album",
/// a scrubber with elapsed and remaining time, and the format and gain on the right.
/// A playback error replaces the text; clicking it shows the full message.
struct TransportDisplay: View {
    let transport: TransportModel
    let artwork: ArtworkStore
    @State private var seekPreview: Double?
    @State private var showingError = false

    var body: some View {
        HStack(spacing: 8) {
            ArtworkView(store: artwork, request: artwork.nowPlayingRequest,
                        identifier: "now-playing-art", dimension: 38)
            if let error = transport.error {
                errorSummary(error)
            } else {
                VStack(spacing: 1) {
                    HStack(spacing: 4) {
                        Text(transport.title).font(.system(size: 12, weight: .semibold)).lineLimit(1)
                            .accessibilityIdentifier("now-playing-title")
                        if let warning = transport.metadataError {
                            Image(systemName: "exclamationmark.triangle.fill").font(.system(size: 9))
                                .foregroundStyle(.orange).help("Track metadata: \(warning)")
                                .accessibilityIdentifier("now-playing-metadata-warning")
                        }
                    }
                    Text(transport.artistAlbum).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                        .accessibilityIdentifier("now-playing-subtitle")
                    HStack(spacing: 6) {
                        Text(seekPreview.map(DisplayFormat.clock) ?? transport.elapsedText)
                            .frame(width: 38, alignment: .trailing)
                            .accessibilityIdentifier("now-playing-elapsed")
                        NativeSeekSlider(transport: transport, preview: $seekPreview,
                                         identifier: "now-playing-scrubber", small: true)
                            .frame(height: 12)
                        Text(seekPreview.map { DisplayFormat.remaining(elapsed: $0, duration: transport.duration) }
                             ?? transport.remainingText)
                            .frame(width: 42, alignment: .leading)
                            .accessibilityIdentifier("now-playing-remaining")
                    }
                    .font(.system(size: 10).monospacedDigit()).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity)
                // "FLAC 16/44.1 · RG album −6.2 dB", broken at the separator when narrow.
                VStack(alignment: .trailing, spacing: 1) {
                    Text(transport.nowPlaying == nil ? "" : transport.format)
                    if transport.nowPlaying != nil, let gain = transport.gainLabel { Text(gain) }
                }
                .font(.system(size: 10)).foregroundStyle(.secondary).lineLimit(1)
                .frame(width: 104, alignment: .trailing)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(transport.formatLine)
                .accessibilityIdentifier("now-playing-format")
            }
        }
        .padding(.horizontal, 5).padding(.vertical, 3)
        .frame(minWidth: 320, idealWidth: 380, maxWidth: 560)
        .frame(height: 46)
        .background(RoundedRectangle(cornerRadius: 6).fill(.background.opacity(0.6)))
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(.separator))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("now-playing-display")
        .onChange(of: transport.currentID) { _, _ in seekPreview = nil }
        .onChange(of: transport.playbackSession) { _, _ in seekPreview = nil }
        .onChange(of: transport.canMutate) { _, enabled in if !enabled { seekPreview = nil } }
        .onChange(of: transport.error) { _, error in if error == nil { showingError = false } }
    }

    private func errorSummary(_ error: String) -> some View {
        Button { showingError = true } label: {
            HStack(spacing: 6) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Playback Error").font(.system(size: 12, weight: .semibold))
                    Text(error).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Show the playback error")
        .accessibilityIdentifier("now-playing-error-summary")
        .popover(isPresented: $showingError, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 8) {
                Text("Playback Error").font(.headline)
                Text(error).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            }
            .padding(14).frame(width: 360)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("now-playing-error")
        }
    }
}

/// AppKit owns native mouse tracking, keyboard focus and accessibility. Continuous
/// mouse actions only preview; the tracking loop's return commits one occurrence-bound seek.
struct NativeSeekSlider: NSViewRepresentable {
    var transport: TransportModel
    @Binding var preview: Double?
    var identifier = "seek-slider"
    var small = false

    func makeCoordinator() -> Coordinator { Coordinator(transport: transport, preview: $preview) }

    func makeNSView(context: Context) -> SeekSlider {
        let slider = SeekSlider()
        slider.minValue = 0
        slider.isContinuous = true
        if small { slider.controlSize = .mini }
        slider.target = context.coordinator
        slider.action = #selector(Coordinator.changed(_:))
        slider.setAccessibilityIdentifier(identifier)
        slider.setAccessibilityLabel("Playback position in seconds")
        slider.beginTracking = { [weak coordinator = context.coordinator] in coordinator?.begin() }
        slider.endTracking = { [weak coordinator = context.coordinator] in coordinator?.end(slider: $0) }
        return slider
    }

    func updateNSView(_ slider: SeekSlider, context: Context) {
        context.coordinator.preview = $preview
        let duration = transport.duration ?? 0
        slider.maxValue = max(duration, 1 / 48_000)
        slider.isEnabled = transport.canMutate && transport.currentID != nil && duration > 0
        context.coordinator.invalidateIfNeeded()
        if !context.coordinator.tracking || context.coordinator.occurrence == nil {
            slider.doubleValue = min(max(transport.elapsed, 0), slider.maxValue)
        }
    }

    @MainActor final class Coordinator: NSObject {
        weak var transport: TransportModel?
        var preview: Binding<Double?>
        var tracking = false
        var occurrence: (entry: UInt64, session: UInt64)?

        init(transport: TransportModel, preview: Binding<Double?>) {
            self.transport = transport
            self.preview = preview
        }

        func begin() {
            tracking = true
            guard let transport, transport.canMutate, let entry = transport.currentID,
                  (transport.duration ?? 0) > 0 else { occurrence = nil; return }
            occurrence = (entry, transport.playbackSession)
        }

        func invalidateIfNeeded() {
            guard let occurrence, let transport else { return }
            if occurrence.entry != transport.currentID
                || occurrence.session != transport.playbackSession || !transport.canMutate
                || (transport.duration ?? 0) <= 0 {
                self.occurrence = nil
            }
        }

        @objc func changed(_ slider: NSSlider) {
            if tracking {
                invalidateIfNeeded()
                if occurrence != nil { preview.wrappedValue = slider.doubleValue }
            } else if let transport, let entry = transport.currentID {
                transport.seek(seconds: slider.doubleValue, entryID: entry)
            }
        }

        func end(slider: NSSlider) {
            invalidateIfNeeded()
            let destination = occurrence
            occurrence = nil
            tracking = false
            preview.wrappedValue = nil
            if let destination {
                transport?.seek(seconds: slider.doubleValue, entryID: destination.entry)
            }
        }
    }
}

final class SeekSlider: NSSlider {
    var beginTracking: (() -> Void)?
    var endTracking: ((NSSlider) -> Void)?

    override func mouseDown(with event: NSEvent) {
        guard isEnabled else { return }
        beginTracking?()
        super.mouseDown(with: event)
        endTracking?(self)
    }

    override func setAccessibilityValue(_ value: Any?) {
        guard isEnabled, let number = value as? NSNumber, number.doubleValue.isFinite else { return }
        doubleValue = min(max(number.doubleValue, minValue), maxValue)
        sendAction(action, to: target)
    }

    override func accessibilityPerformIncrement() -> Bool { adjust(by: 5) }
    override func accessibilityPerformDecrement() -> Bool { adjust(by: -5) }

    private func adjust(by seconds: Double) -> Bool {
        guard isEnabled else { return false }
        doubleValue = min(max(doubleValue + seconds, minValue), maxValue)
        sendAction(action, to: target)
        return true
    }
}

/// Output volume, 0–100. Every change is sent; `TransportModel` coalesces them.
struct VolumeSlider: NSViewRepresentable {
    var transport: TransportModel

    func makeCoordinator() -> Coordinator { Coordinator(transport: transport) }

    func makeNSView(context: Context) -> AccessibleSlider {
        let slider = AccessibleSlider()
        slider.minValue = 0
        slider.maxValue = 100
        slider.isContinuous = true
        slider.controlSize = .small
        slider.target = context.coordinator
        slider.action = #selector(Coordinator.changed(_:))
        slider.setAccessibilityIdentifier("transport-volume")
        slider.setAccessibilityLabel("Volume")
        slider.toolTip = "Volume"
        return slider
    }

    func updateNSView(_ slider: AccessibleSlider, context: Context) {
        context.coordinator.transport = transport
        slider.isEnabled = transport.canSetVolume
        if slider.doubleValue.rounded() != Double(transport.volume) {
            slider.doubleValue = Double(transport.volume)
        }
    }

    @MainActor final class Coordinator: NSObject {
        weak var transport: TransportModel?
        init(transport: TransportModel) { self.transport = transport }
        @objc func changed(_ slider: NSSlider) {
            transport?.setVolume(Int(slider.doubleValue.rounded()))
        }
    }
}

/// An NSSlider whose accessibility value and increments go through its action.
final class AccessibleSlider: NSSlider {
    override func setAccessibilityValue(_ value: Any?) {
        guard isEnabled, let number = value as? NSNumber, number.doubleValue.isFinite else { return }
        doubleValue = min(max(number.doubleValue, minValue), maxValue)
        sendAction(action, to: target)
    }

    override func accessibilityPerformIncrement() -> Bool { adjust(by: 5) }
    override func accessibilityPerformDecrement() -> Bool { adjust(by: -5) }

    private func adjust(by step: Double) -> Bool {
        guard isEnabled else { return false }
        doubleValue = min(max(doubleValue + step, minValue), maxValue)
        sendAction(action, to: target)
        return true
    }
}
