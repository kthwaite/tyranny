import AppKit
import SwiftUI
import TyrannyCore

// The player bar's native sliders: the seek scrubber and volume.

/// AppKit owns native mouse tracking, keyboard focus and accessibility. Continuous
/// mouse actions only preview; the tracking loop's return commits one occurrence-bound seek.
struct NativeSeekSlider: NSViewRepresentable {
    var transport: TransportModel
    @Binding var preview: Double?
    var identifier = "seek-slider"

    func makeCoordinator() -> Coordinator { Coordinator(transport: transport, preview: $preview) }

    func makeNSView(context: Context) -> SeekSlider {
        let slider = SeekSlider()
        slider.minValue = 0
        slider.isContinuous = true
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
